//! The open conversation: the SDK's timeline of a room, told to the UI as rows and their changes.
//! The SDK keeps the events, applies edits, reactions and deletions, counts poll votes, places
//! read receipts and holds messages that are still on their way out; this file only says what
//! each of its items looks like as a row.

use std::{
    collections::{BTreeSet, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Room,
    ruma::{
        EventId, OwnedEventId, OwnedRoomId, OwnedTransactionId, OwnedUserId, UserId,
        events::{
            AnySyncStateEvent, AnySyncTimelineEvent, StateEventContentChange, SyncStateEvent,
            room::{member::MembershipChange as Change, message::MessageType},
        },
        room_version_rules::RoomVersionRules,
    },
};
use matrix_sdk_ui::eyeball_im::VectorDiff;
use matrix_sdk_ui::timeline::{
    AnyOtherStateEventContentChange, EncryptedMessage, EventSendState, EventTimelineItem, InReplyToDetails, MembershipChange, MsgLikeKind, PollState,
    Profile, RoomMembershipChange, Timeline, TimelineBuilder, TimelineDetails, TimelineEventItemId, TimelineItem, TimelineItemContent,
    TimelineItemKind, TimelineReadReceiptTracking, VirtualTimelineItem, default_event_filter,
};
use tokio::sync::{broadcast, mpsc};

use super::{
    AbortOnDrop, DELETED, Delivery, Ev, Kind, MAX_KEY_FETCHES, Msg, Place, Poll, Quote, Reaction, UTD, content, display_name, link_preview, mark_read,
    marks, network, other_person, plain, sticker,
};

/// Beeper's bridges say in events of this type what the network did with a message.
const SEND_STATUS: &str = "com.beeper.message_send_status";
/// People named as typing, readers kept per row, replies whose original is asked for: each is
/// someone else's data and gets a ceiling.
const MAX_TYPING: usize = 8;
const MAX_SEEN: usize = 32;
const MAX_QUOTES_FETCHED: usize = 256;
/// Sessions whose key was asked of the backup while a conversation is open.
const MAX_SESSIONS_FETCHED: usize = 256;
/// Longest chat name or failure reason put on a row.
const LINE_MAX: usize = 120;

/// The conversation on screen. Dropping it stops following the room.
pub(super) struct Open {
    pub room: OwnedRoomId,
    pub timeline: Arc<Timeline>,
    _tasks: [AbortOnDrop; 2],
}

/// Who is looking at the conversation: what a row needs to know besides its own item.
struct Viewer {
    own: OwnedUserId,
    /// Bridge bots and the like: present in the room, never people in the chat.
    service: BTreeSet<OwnedUserId>,
    /// The chat is on another network, where a message has arrived only once its bridge says so.
    bridged: bool,
}

/// Starts following a room: sends the rows it has now, then every change to them, and who is
/// typing. While the window is in front, a message that arrives is marked read.
pub(super) async fn open(room: &Room, ev: &mpsc::Sender<Ev>, incognito: Arc<AtomicBool>) -> Result<Open> {
    let dm = other_person(room).await.is_some();
    // Past this many people, who comes and goes is noise: Beeper draws the same line.
    let big = room.joined_members_count() >= 100;
    let viewer = Arc::new(Viewer {
        own: room.own_user_id().to_owned(),
        service: room.service_members().unwrap_or_default(),
        bridged: network(room).await.is_some(),
    });
    let timeline = TimelineBuilder::new(room)
        .track_read_marker_and_receipts(TimelineReadReceiptTracking::AllEvents)
        .event_filter({
            let viewer = viewer.clone();
            move |event, rules| shown(event, rules, dm, big, &viewer)
        })
        .build()
        .await?;
    let timeline = Arc::new(timeline);
    let id = room.room_id().to_owned();
    let (items, diffs) = timeline.subscribe().await;
    // What is there already goes the same way as every change after it.
    let diffs = futures_util::stream::once(std::future::ready(vec![VectorDiff::Reset { values: items }])).chain(diffs);

    let rows = tokio::spawn({
        let (timeline, ev, id, viewer) = (timeline.clone(), ev.clone(), id.clone(), viewer.clone());
        async move {
            let mut diffs = std::pin::pin!(diffs);
            let (mut asked, mut keyed): (HashSet<OwnedEventId>, HashSet<String>) = Default::default();
            while let Some(batch) = diffs.next().await {
                let arrived = batch.iter().any(|diff| matches!(diff, VectorDiff::PushBack { value } if from_someone_else(value)));
                let quotes: Vec<OwnedEventId> =
                    batch.iter().flat_map(touched).filter_map(unknown_quote).filter(|id| asked.len() < MAX_QUOTES_FETCHED && asked.insert(id.clone())).collect();
                let sessions: Vec<String> = batch.iter().flat_map(touched).filter_map(|item| locked(item)).filter(|s| !keyed.contains(s)).collect();
                let diffs = batch.into_iter().map(|diff| diff.map(|item| row(&item, &viewer))).collect();
                if ev.send(Ev::Timeline { room: id.clone(), diffs }).await.is_err() {
                    break;
                }
                // Asked once per session, and only once the backup can answer: recovery asks again.
                if !sessions.is_empty() && keyed.len() < MAX_SESSIONS_FETCHED && timeline.room().client().encryption().backups().are_enabled().await {
                    keyed.extend(sessions.iter().cloned());
                    fetch_keys(timeline.room(), sessions).await;
                }
                for quoting in quotes {
                    // The answer comes back as a change to that row.
                    if let Err(e) = timeline.fetch_details_for_event(&quoting).await {
                        log::debug!("quoted message: {e}");
                    }
                }
                if arrived && crate::FOREGROUND.load(Ordering::Relaxed) {
                    let newest = timeline.latest_event_id().await;
                    if let Err(e) = mark_read(timeline.room(), newest, incognito.load(Ordering::Relaxed)).await {
                        log::warn!("mark read: {e:#}");
                    }
                }
            }
        }
    });
    let typing = tokio::spawn({
        let (room, ev, viewer) = (room.clone(), ev.clone(), viewer);
        let (guard, mut typing) = room.subscribe_to_typing_notifications();
        async move {
            let _guard = guard;
            loop {
                let users = match typing.recv().await {
                    Ok(users) => users,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let mut names = Vec::new();
                for user in users.iter().filter(|u| !viewer.service.contains(*u)).take(MAX_TYPING) {
                    names.push(display_name(&room, user).await);
                }
                if ev.send(Ev::Typing { room: room.room_id().to_owned(), names }).await.is_err() {
                    break;
                }
            }
        }
    });
    Ok(Open { room: id, timeline, _tasks: [AbortOnDrop(rows), AbortOnDrop(typing)] })
}

/// The items a change brings: new ones, and ones that took the place of an old one.
fn touched(diff: &VectorDiff<Arc<TimelineItem>>) -> Vec<&Arc<TimelineItem>> {
    match diff {
        VectorDiff::Append { values } | VectorDiff::Reset { values } => values.iter().collect(),
        VectorDiff::PushBack { value }
        | VectorDiff::PushFront { value }
        | VectorDiff::Insert { value, .. }
        | VectorDiff::Set { value, .. } => vec![value],
        _ => Vec::new(),
    }
}

fn from_someone_else(item: &Arc<TimelineItem>) -> bool {
    item.as_event().is_some_and(|event| event.is_remote_event() && !event.is_own())
}

/// The event of a reply whose original is not among the loaded messages and was not asked for.
fn unknown_quote(item: &Arc<TimelineItem>) -> Option<OwnedEventId> {
    let event = item.as_event()?;
    let TimelineItemContent::MsgLike(like) = event.content() else { return None };
    let unknown = matches!(like.in_reply_to.as_ref()?.event, TimelineDetails::Unavailable);
    event.event_id().filter(|_| unknown).map(ToOwned::to_owned)
}

/// The session of a message that could not be decrypted.
fn locked(item: &TimelineItem) -> Option<String> {
    let TimelineItemContent::MsgLike(like) = item.as_event()?.content() else { return None };
    match &like.kind {
        MsgLikeKind::UnableToDecrypt(EncryptedMessage::MegolmV1AesSha2 { session_id, .. }) => Some(session_id.clone()),
        _ => None,
    }
}

/// Gets the keys of these sessions from the server-side backup; the timeline then reads the
/// messages by itself. Does nothing until recovery has unlocked the backup.
async fn fetch_keys(room: &Room, sessions: impl IntoIterator<Item = String>) {
    let backups = room.client().encryption().backups();
    let sessions: BTreeSet<String> = sessions.into_iter().collect();
    for session in sessions.iter().take(MAX_KEY_FETCHES) {
        if let Err(e) = backups.download_room_key(room.room_id(), session).await {
            log::warn!("backup key for {}: {e}", room.room_id());
            break;
        }
    }
}

/// After recovery: the messages of the open conversation that were locked can now be read.
pub(super) async fn unlock(timeline: &Timeline) {
    let items = timeline.items().await;
    fetch_keys(timeline.room(), items.iter().filter_map(|item| locked(item))).await;
}

/// Which message a command means: an event, or one that has not left yet.
pub(super) fn item_id(id: &str) -> Result<TimelineEventItemId> {
    Ok(if id.starts_with('$') {
        TimelineEventItemId::EventId(EventId::parse(id)?)
    } else {
        TimelineEventItemId::TransactionId(OwnedTransactionId::from(id.to_owned()))
    })
}

/// Sends again a message whose sending failed.
pub(super) async fn retry(timeline: &Timeline, id: &str) -> Result<()> {
    let items = timeline.items().await;
    let handle = items
        .iter()
        .rev()
        .filter_map(|item| item.as_event())
        .find(|event| event.transaction_id().is_some_and(|txn| txn.as_str() == id))
        .and_then(EventTimelineItem::local_echo_send_handle)
        .context("That message is no longer waiting to be sent")?;
    Ok(handle.unwedge().await?)
}

/// Whether an event gets a row. On top of what the SDK shows by default:
/// - a bridge's word on a sent message rides along as a row that draws nothing;
/// - a one-to-one chat has no lines about the chat itself;
/// - people joining, leaving and being removed get a line (in a chat of a hundred or more only
///   the user does), bots do not, and neither does a change of someone's name or picture;
/// - a new name, picture or description of the chat gets a line, their first setting does not:
///   a bridge sets all three for every chat it creates.
fn shown(event: &AnySyncTimelineEvent, rules: &RoomVersionRules, dm: bool, big: bool, viewer: &Viewer) -> bool {
    let service = &viewer.service;
    match event {
        AnySyncTimelineEvent::MessageLike(message) if message.event_type().to_string() == SEND_STATUS => true,
        AnySyncTimelineEvent::MessageLike(_) => default_event_filter(event, rules),
        AnySyncTimelineEvent::State(_) if dm => false,
        AnySyncTimelineEvent::State(AnySyncStateEvent::RoomMember(SyncStateEvent::Original(member))) => {
            let by_bot = service.contains(&member.sender);
            !service.contains(&member.state_key)
                && (!big || member.state_key == viewer.own)
                && match member.membership_change() {
                    Change::Joined | Change::InvitationAccepted | Change::Left | Change::Kicked | Change::KickedAndBanned | Change::Banned => true,
                    // A bridge invites people on its way to joining them.
                    Change::Invited => !by_bot,
                    _ => false,
                }
        }
        AnySyncTimelineEvent::State(AnySyncStateEvent::RoomName(SyncStateEvent::Original(e))) => e.unsigned.prev_content.is_some(),
        AnySyncTimelineEvent::State(AnySyncStateEvent::RoomAvatar(SyncStateEvent::Original(e))) => e.unsigned.prev_content.is_some(),
        AnySyncTimelineEvent::State(AnySyncStateEvent::RoomTopic(SyncStateEvent::Original(e))) => e.unsigned.prev_content.is_some(),
        AnySyncTimelineEvent::State(_) => false,
    }
}

/// An event that shows as a row of its own: not a state of delivery, which only marks its message.
pub fn drawn(item: &TimelineItem) -> bool {
    item.as_event().is_some_and(|e| !matches!(e.content(), TimelineItemContent::MsgLike(like) if matches!(like.kind, MsgLikeKind::Other(_))))
}

/// A timeline item as a row.
fn row(item: &TimelineItem, viewer: &Viewer) -> Msg {
    match item.kind() {
        TimelineItemKind::Event(event) => event_row(event, viewer),
        TimelineItemKind::Virtual(marker) => {
            let (kind, ts) = match marker {
                VirtualTimelineItem::DateDivider(ts) => (Kind::Day, u64::from(ts.0)),
                VirtualTimelineItem::ReadMarker => (Kind::Unread, 0),
                VirtualTimelineItem::TimelineStart => (Kind::Hidden, 0),
            };
            Msg { ts, ..Msg::bare(item.unique_id().0.clone(), kind, viewer.own.clone()) }
        }
    }
}

fn event_row(event: &EventTimelineItem, viewer: &Viewer) -> Msg {
    let id = match event.identifier() {
        TimelineEventItemId::EventId(id) => id.to_string(),
        TimelineEventItemId::TransactionId(id) => id.to_string(),
    };
    let mut m = Msg::bare(id, Kind::Message, event.sender().to_owned());
    m.sender = name(event.sender_profile(), event.sender());
    m.own = event.is_own();
    m.ts = u64::from(event.timestamp().0);
    let json = content_json(event);
    let flag = |key: &str| json.as_ref().and_then(|c| c.get(key)).and_then(serde_json::Value::as_bool) == Some(true);
    match event.content() {
        TimelineItemContent::MsgLike(like) => {
            fill(&mut m, &like.kind, &viewer.own);
            m.reply = like.in_reply_to.as_ref().map(|reply| quote(reply, &viewer.own));
            m.reactions =
                like.reactions.iter().map(|(key, by)| Reaction { key: key.clone(), count: by.len(), own: by.contains_key(&viewer.own) }).collect();
            match &like.kind {
                MsgLikeKind::Message(_) => m.link = json.as_ref().and_then(link_preview),
                // The filter lets one custom type through.
                MsgLikeKind::Other(_) => {
                    m.kind = json.as_ref().and_then(send_status).map_or(Kind::Hidden, |(target, delivery)| Kind::Status { target, delivery });
                }
                _ => {}
            }
        }
        // Beeper's bridges mark the memberships they make up for a chat's bookkeeping.
        TimelineItemContent::MembershipChange(_)
            if ["com.beeper.exclude_from_timeline", "com.beeper.bridge.is_network_bot", "com.beeper.bridge.is_bridge_bot"].into_iter().any(flag) =>
        {
            m.kind = Kind::Hidden;
        }
        TimelineItemContent::MembershipChange(change) => membership(&mut m, change, viewer),
        TimelineItemContent::OtherState(state) => {
            let actor = (!viewer.service.contains(event.sender())).then_some(if m.own { "You" } else { m.sender.as_str() });
            match state_line(state.content(), actor) {
                Some(line) => (m.kind, m.body) = (Kind::Notice, line),
                None => m.kind = Kind::Hidden,
            }
        }
        _ => m.kind = Kind::Hidden,
    }
    m.delivery = match event.send_state() {
        Some(EventSendState::NotSentYet { .. }) => Delivery::Sending,
        Some(EventSendState::SendingFailed { error, .. }) => Delivery::Failed(clip(&plain(error))),
        // On another network it has arrived once the bridge says so (a `Status` row), unless that
        // network's own app sent it, which the bridge marks.
        _ if m.own && viewer.bridged && !json.as_ref().is_some_and(|c| c.get("fi.mau.double_puppet_source").is_some()) => Delivery::Sending,
        _ => Delivery::Sent,
    };
    m.seen = event.read_receipts().keys().filter(|user| **user != viewer.own && !viewer.service.contains(*user)).take(MAX_SEEN).cloned().collect();
    m
}

/// The content of the event behind an item, as the server sent it; for an edited message, as it
/// now reads.
fn content_json(event: &EventTimelineItem) -> Option<serde_json::Value> {
    let content: serde_json::Value = event.latest_json()?.get_field("content").ok()??;
    Some(match content.get("m.new_content") {
        Some(new) => new.clone(),
        None => content,
    })
}

fn name(profile: &TimelineDetails<Profile>, user: &UserId) -> String {
    match profile {
        TimelineDetails::Ready(Profile { display_name: Some(name), .. }) => name.clone(),
        _ => user.localpart().to_owned(),
    }
}

fn clip(text: &str) -> String {
    text.chars().take(LINE_MAX).collect()
}

/// What a message-like item puts on a row. Also tells what a quoted message says.
fn fill(m: &mut Msg, kind: &MsgLikeKind, own: &UserId) {
    match kind {
        MsgLikeKind::Message(message) => {
            let (media, text) = content(message.msgtype());
            m.media = media;
            if let MessageType::Location(place) = message.msgtype() {
                m.place = Place::parse(&place.geo_uri, &place.body);
            }
            // The place's description is told with the place.
            m.body = if m.place.is_some() { String::new() } else { text };
            (m.mentions, m.mentioned) = marks(&m.body, message.msgtype(), message.mentions(), own);
            m.mentioned &= !m.own;
            m.edited = message.is_edited();
        }
        MsgLikeKind::Sticker(sent) => m.media = Some(sticker(sent.content())),
        MsgLikeKind::Poll(state) => m.poll = Some(poll(state)),
        MsgLikeKind::Redacted => (m.body, m.deleted) = (DELETED.to_owned(), true),
        MsgLikeKind::UnableToDecrypt(_) => m.body = UTD.to_owned(),
        MsgLikeKind::Other(_) | MsgLikeKind::LiveLocation(_) => m.kind = Kind::Hidden,
    }
}

fn quote(reply: &InReplyToDetails, own: &UserId) -> Quote {
    let (sender, text) = match &reply.event {
        TimelineDetails::Ready(quoted) => {
            let mut m = Msg::bare(String::new(), Kind::Message, quoted.sender.clone());
            if let TimelineItemContent::MsgLike(like) = &quoted.content {
                fill(&mut m, &like.kind, own);
            }
            (name(&quoted.sender_profile, &quoted.sender), m.summary())
        }
        _ => (String::new(), "Message unavailable".to_owned()),
    };
    Quote { id: reply.event_id.to_string(), sender, text }
}

fn poll(state: &PollState) -> Poll {
    let counted = state.results();
    let answers = counted.answers.into_iter().map(|answer| (answer.id, answer.text));
    Poll::counted(&counted.question, answers, counted.max_selections, counted.end_time.map(|t| u64::from(t.0)), counted.votes)
}

fn membership(m: &mut Msg, change: &RoomMembershipChange, viewer: &Viewer) {
    let me = change.user_id() == viewer.own;
    let subject = if me { "You".to_owned() } else { change.display_name().unwrap_or_else(|| change.user_id().localpart().to_owned()) };
    // Something a bot did was done by someone on the other network, who is not named.
    let actor = (!viewer.service.contains(&m.from) && m.from != change.user_id()).then_some(if m.own { "You" } else { m.sender.as_str() });
    match change.change() {
        Some(MembershipChange::Joined | MembershipChange::InvitationAccepted) => (m.kind, m.sender) = (Kind::Joined, subject),
        Some(change) => match membership_line(change, actor, &subject, me) {
            Some(line) => (m.kind, m.body) = (Kind::Notice, line),
            None => m.kind = Kind::Hidden,
        },
        None => m.kind = Kind::Hidden,
    }
}

/// A change of someone's membership as a line; `None` for the ones that get no line. `actor` is
/// whoever did it to `subject`, when that is someone else and known.
fn membership_line(change: MembershipChange, actor: Option<&str>, subject: &str, me: bool) -> Option<String> {
    let object = if me { "you" } else { subject };
    Some(match (change, actor) {
        (MembershipChange::Left, _) => format!("{subject} left"),
        (MembershipChange::Kicked | MembershipChange::KickedAndBanned | MembershipChange::Banned, Some(actor)) => format!("{actor} removed {object}"),
        (MembershipChange::Kicked | MembershipChange::KickedAndBanned | MembershipChange::Banned, None) => {
            format!("{subject} {} removed", if me { "were" } else { "was" })
        }
        (MembershipChange::Invited, Some(actor)) => format!("{actor} invited {object}"),
        _ => return None,
    })
}

/// A change to the chat itself as a line.
fn state_line(change: &AnyOtherStateEventContentChange, actor: Option<&str>) -> Option<String> {
    let (what, to) = match change {
        AnyOtherStateEventContentChange::RoomName(StateEventContentChange::Original { content, .. }) if !content.name.is_empty() => {
            ("name", format!(" to \u{201c}{}\u{201d}", clip(&content.name)))
        }
        AnyOtherStateEventContentChange::RoomAvatar(_) => ("picture", String::new()),
        AnyOtherStateEventContentChange::RoomTopic(_) => ("description", String::new()),
        _ => return None,
    };
    Some(match actor {
        Some(actor) => format!("{actor} changed the {what}{to}"),
        None => format!("The {what} was changed{to}"),
    })
}

/// What a bridge's status event says, and of which message.
fn send_status(content: &serde_json::Value) -> Option<(String, Delivery)> {
    let text = |key: &str| content.get(key).and_then(serde_json::Value::as_str);
    let target = content.get("m.relates_to")?.get("event_id")?.as_str()?.to_owned();
    let delivery = match text("status")? {
        "SUCCESS" if content.get("delivered_to_users").and_then(serde_json::Value::as_array).is_some_and(|to| !to.is_empty()) => Delivery::Delivered,
        "SUCCESS" => Delivery::Sent,
        "PENDING" => Delivery::Sending,
        "FAIL_RETRIABLE" | "FAIL_PERMANENT" => Delivery::Failed(clip(text("message").unwrap_or_default())),
        _ => return None,
    };
    Some((target, delivery))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_bridge_status_names_its_message_and_what_became_of_it() {
        let status = |extra: serde_json::Value| {
            let mut content = json!({ "m.relates_to": { "rel_type": "m.reference", "event_id": "$m" } });
            content.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            send_status(&content)
        };
        assert_eq!(status(json!({ "status": "SUCCESS" })), Some(("$m".to_owned(), Delivery::Sent)));
        assert_eq!(status(json!({ "status": "SUCCESS", "delivered_to_users": [] })).unwrap().1, Delivery::Sent);
        assert_eq!(status(json!({ "status": "SUCCESS", "delivered_to_users": ["@a:x"] })).unwrap().1, Delivery::Delivered);
        assert_eq!(status(json!({ "status": "PENDING" })).unwrap().1, Delivery::Sending);
        let failed = status(json!({ "status": "FAIL_PERMANENT", "reason": "m.foreign_network_error", "message": "x".repeat(500) }));
        assert_eq!(failed.unwrap().1, Delivery::Failed("x".repeat(LINE_MAX)));
        assert_eq!(status(json!({ "status": "SOMETHING_NEW" })), None);
        assert_eq!(send_status(&json!({ "status": "SUCCESS" })), None, "about no message");
    }

    #[test]
    fn membership_changes_read_as_sentences() {
        let line = |change, actor, subject, me| membership_line(change, actor, subject, me);
        assert_eq!(line(MembershipChange::Left, None, "Ann", false).as_deref(), Some("Ann left"));
        assert_eq!(line(MembershipChange::Kicked, Some("Bob"), "Ann", false).as_deref(), Some("Bob removed Ann"));
        assert_eq!(line(MembershipChange::Banned, Some("Bob"), "You", true).as_deref(), Some("Bob removed you"));
        assert_eq!(line(MembershipChange::Kicked, None, "You", true).as_deref(), Some("You were removed"));
        assert_eq!(line(MembershipChange::Kicked, None, "Ann", false).as_deref(), Some("Ann was removed"));
        assert_eq!(line(MembershipChange::Invited, Some("You"), "Ann", false).as_deref(), Some("You invited Ann"));
        // A bridge's invitation is bookkeeping; a rejected one is nobody's news.
        assert_eq!(line(MembershipChange::Invited, None, "Ann", false), None);
        assert_eq!(line(MembershipChange::InvitationRejected, None, "Ann", false), None);
    }
}
