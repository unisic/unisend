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
    event_handler::EventHandlerDropGuard,
    ruma::{
        EventId, OwnedEventId, OwnedRoomId, OwnedTransactionId, OwnedUserId, UserId,
        events::{
            AnySyncMessageLikeEvent, AnySyncStateEvent, AnySyncTimelineEvent, StateEventContentChange, SyncMessageLikeEvent, SyncStateEvent,
            receipt::{ReceiptThread, ReceiptType},
            room::{
                member::MembershipChange as Change,
                message::{MessageType, Relation},
            },
        },
        room_version_rules::RoomVersionRules,
        serde::Raw,
    },
};
use matrix_sdk_ui::eyeball_im::{Vector, VectorDiff};
use matrix_sdk_ui::timeline::{
    AnyOtherStateEventContentChange, EncryptedMessage, EventSendState, EventTimelineItem, InReplyToDetails, MembershipChange, MsgLikeKind, PollState,
    Profile, RoomMembershipChange, Timeline, TimelineBuilder, TimelineDetails, TimelineEventItemId, TimelineItem, TimelineItemContent,
    TimelineItemKind, TimelineReadReceiptTracking, VirtualTimelineItem, default_event_filter,
};
use tokio::sync::{broadcast, mpsc};

use super::{
    AbortOnDrop, deleted, utd, Delivery, Ev, Kind, OwnRead, MAX_KEY_FETCHES, Msg, Place, Poll, Quote, QuoteState, Reaction, content, display_name, link_preview, mark_read,
    format, marks, bridge_names, other_person, plain, sticker, disappear, features,
};
use crate::i18n::t;

/// Beeper's bridges say in events of this type what the network did with a message.
const SEND_STATUS: &str = "com.beeper.message_send_status";
const STABLE_POLL: &str = "m.poll.start";
/// People named as typing, readers kept per row, replies whose original is asked for: each is
/// someone else's data and gets a ceiling.
const MAX_TYPING: usize = 8;
const MAX_SEEN: usize = 32;
const MAX_QUOTES_FETCHED: usize = 256;
/// Sessions whose key was asked of the backup while a conversation is open.
const MAX_SESSIONS_FETCHED: usize = 256;
/// Batches of lookups waiting for the ones before them, and quoted messages fetched at once.
const LOOKUP_QUEUE: usize = 16;
const QUOTES_AT_ONCE: usize = 4;
/// Longest chat name or failure reason put on a row.
const LINE_MAX: usize = 120;

/// The conversation on screen. Dropping it stops following the room.
pub(super) struct Open {
    pub room: OwnedRoomId,
    pub timeline: Arc<Timeline>,
    /// Builds every row again, as after a change of the interface language.
    pub relabel: Arc<tokio::sync::Notify>,
    _tasks: [AbortOnDrop; 4],
    _features: EventHandlerDropGuard,
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
        bridged: bridge_names(room).await.0.is_some(),
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

    // On a phone every lookup is a round trip: made beside the rows, a chat full of replies or of
    // locked messages does not hold back the rows after them.
    let (lookups, mut wanted) = mpsc::channel::<(Vec<String>, Vec<OwnedEventId>)>(LOOKUP_QUEUE);
    let lookup = tokio::spawn({
        let timeline = timeline.clone();
        async move {
            while let Some((sessions, quotes)) = wanted.recv().await {
                fetch_keys(timeline.room(), sessions).await;
                // The answers come back as changes to those rows.
                futures_util::stream::iter(quotes)
                    .for_each_concurrent(QUOTES_AT_ONCE, |quoting| {
                        let timeline = &timeline;
                        async move {
                            if let Err(e) = timeline.fetch_details_for_event(&quoting).await {
                                log::debug!("quoted message: {e}");
                            }
                        }
                    })
                    .await;
            }
        }
    });
    let relabel = Arc::new(tokio::sync::Notify::new());
    // A receipt on an event the timeline had not loaded yet gets its row only once it is loaded.
    let rows_changed = Arc::new(tokio::sync::Notify::new());
    let rows = tokio::spawn({
        let (timeline, ev, id, viewer, relabel, rows_changed) = (timeline.clone(), ev.clone(), id.clone(), viewer.clone(), relabel.clone(), rows_changed.clone());
        async move {
            let mut diffs = std::pin::pin!(diffs);
            let (mut asked, mut keyed): (HashSet<OwnedEventId>, HashSet<String>) = Default::default();
            let mut first = true;
            // The items as the screen has them, so that each can be built again in place, without a refetch.
            let mut shown = Vector::new();
            loop {
                let batch = tokio::select! {
                    batch = diffs.next() => match batch {
                        Some(batch) => batch,
                        None => break,
                    },
                    () = relabel.notified() => shown.iter().cloned().enumerate().map(|(index, value)| VectorDiff::Set { index, value }).collect(),
                };
                batch.iter().cloned().for_each(|diff| diff.apply(&mut shown));
                let timer_changed = std::mem::take(&mut first) || batch.iter().flat_map(touched).any(|item| sets_timer(item));
                let arrived = batch.iter().any(|diff| matches!(diff, VectorDiff::PushBack { value } if from_someone_else(value)));
                let quotes: Vec<OwnedEventId> =
                    batch.iter().flat_map(touched).filter_map(unknown_quote).filter(|id| asked.len() < MAX_QUOTES_FETCHED && asked.insert(id.clone())).collect();
                let sessions: Vec<String> = batch.iter().flat_map(touched).filter_map(|item| locked(item)).filter(|s| !keyed.contains(s)).collect();
                let diffs: Vec<_> = batch.into_iter().map(|diff| diff.map(|item| row(&item, &viewer))).collect();
                super::search::rows(&id, &diffs).await;
                if ev.send(Ev::Timeline { room: id.clone(), diffs }).await.is_err() {
                    break;
                }
                rows_changed.notify_one();
                if timer_changed {
                    let setting = disappear::read(timeline.room()).await;
                    if ev.send(Ev::Disappear { room: id.clone(), setting }).await.is_err() {
                        break;
                    }
                }
                // Asked once per session, and only once the backup can answer: recovery asks again.
                let sessions = if !sessions.is_empty() && keyed.len() < MAX_SESSIONS_FETCHED && timeline.room().client().encryption().backups().are_enabled().await {
                    keyed.extend(sessions.iter().cloned());
                    sessions
                } else {
                    Vec::new()
                };
                if !sessions.is_empty() || !quotes.is_empty() {
                    let _ = lookups.send((sessions, quotes)).await;
                }
                if arrived && crate::FOREGROUND.load(Ordering::Relaxed) && !crate::LOCKED.load(Ordering::Relaxed) {
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
    let reads = tokio::spawn({
        let (timeline, ev, id) = (timeline.clone(), ev.clone(), id.clone());
        async move {
            let mut changed = std::pin::pin!(timeline.subscribe_own_user_read_receipts_changed().await);
            let mut last = None;
            loop {
                let read = own_read(&timeline).await;
                if last.as_ref() != Some(&read) {
                    last = Some(read.clone());
                    if ev.send(Ev::OwnRead { room: id.clone(), read }).await.is_err() {
                        break;
                    }
                }
                tokio::select! {
                    next = changed.next() => if next.is_none() { break },
                    () = rows_changed.notified() => {}
                }
            }
        }
    });
    let _features = follow_features(room, ev).await;
    Ok(Open { room: id, timeline, relabel, _tasks: [AbortOnDrop(rows), AbortOnDrop(lookup), AbortOnDrop(typing), AbortOnDrop(reads)], _features })
}

/// Reads only what the SDK keeps; never sends a receipt.
async fn own_read(timeline: &Timeline) -> OwnRead {
    let room = timeline.room();
    let own = room.own_user_id();
    let row = timeline.latest_user_read_receipt_timeline_event_id(own).await.map(String::from);
    let newest = timeline.latest_user_read_receipt(own).await.map(|(id, _)| id);
    // As the SDK looks it up: unthreaded first, then the main thread.
    let mut public = room.load_user_receipt(ReceiptType::Read, &ReceiptThread::Unthreaded, own).await.ok().flatten();
    if public.is_none() {
        public = room.load_user_receipt(ReceiptType::Read, &ReceiptThread::Main, own).await.ok().flatten();
    }
    let public = public.map(|(id, receipt)| {
        let at = receipt.ts.map_or(0, |ts| u64::from(ts.0));
        // The newest receipt being the public one, it falls back to the same shown row.
        (id.to_string(), row.clone().filter(|_| newest.as_ref() == Some(&id)), at)
    });
    OwnRead { read: newest.map(|id| (id.to_string(), row)), public }
}

/// Sends the room's features now and again whenever sync brings the event or a change of power levels; the SDK has stored
/// the state by the time it calls the handler.
async fn follow_features(room: &Room, ev: &mpsc::Sender<Ev>) -> EventHandlerDropGuard {
    let _ = ev.send(Ev::Features { room: room.room_id().to_owned(), features: features::read(room).await }).await;
    let ev = ev.clone();
    let handle = room.client().add_room_event_handler(room.room_id(), move |raw: Raw<AnySyncStateEvent>, room: Room| {
        let ev = ev.clone();
        async move {
            if matches!(raw.get_field::<String>("type").ok().flatten().as_deref(), Some(features::KIND | "m.room.power_levels")) {
                let _ = ev.send(Ev::Features { room: room.room_id().to_owned(), features: features::read(&room).await }).await;
            }
        }
    });
    room.client().event_handler_drop_guard(handle)
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

fn sets_timer(item: &TimelineItem) -> bool {
    item.as_event()
        .is_some_and(|event| matches!(event.content(), TimelineItemContent::OtherState(state) if state.content().event_type().to_string() == disappear::TIMER))
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
            log::warn!("backup key: {e}");
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
        .context(t!("That message is no longer waiting to be sent"))?;
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
        AnySyncTimelineEvent::MessageLike(_) if default_event_filter(event, rules) => true,
        // The SDK reads only the unstable poll; a stable one is shown from its content, without votes.
        AnySyncTimelineEvent::MessageLike(message) if message.event_type().to_string() == STABLE_POLL => true,
        // A message type the SDK does not know is read by its body, as the chat list reads it.
        AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(e))) => {
            !matches!(e.content.relates_to, Some(Relation::Replacement(_))) && !e.content.body().trim().is_empty()
        }
        AnySyncTimelineEvent::MessageLike(_) => false,
        // Told in a one-to-one chat too: whether what is written there stays is the chat's business.
        AnySyncTimelineEvent::State(state) if state.event_type().to_string() == disappear::TIMER => true,
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
    item.as_event().is_some_and(|e| !matches!(e.content(), TimelineItemContent::MsgLike(like) if matches!(&like.kind, MsgLikeKind::Other(o) if o.event_type().to_string() == SEND_STATUS)))
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
            m.reactions = like
                .reactions
                .iter()
                .map(|(key, by)| Reaction {
                    key: key.clone(),
                    count: by.len(),
                    own: by.contains_key(&viewer.own),
                    by: by.keys().filter(|user| **user != viewer.own).take(MAX_SEEN).cloned().collect(),
                })
                .collect();
            let timer = json.as_ref().and_then(|c| c.get(disappear::TIMER)).map(disappear::timer);
            m.disappear = timer.map_or(0, |(ms, _)| ms);
            match &like.kind {
                MsgLikeKind::Message(_) if json.as_ref().is_some_and(disappear::repeats_state) => m.kind = Kind::Hidden,
                MsgLikeKind::Message(_) => m.link = json.as_ref().and_then(link_preview),
                MsgLikeKind::Other(other) if other.event_type().to_string() == STABLE_POLL => {
                    m.poll = json.as_ref().and_then(|c| Poll::parse(c, false));
                    m.kind = if m.poll.is_some() { Kind::Message } else { Kind::Hidden };
                }
                // The filter lets one custom type through.
                MsgLikeKind::Other(_) => {
                    m.kind = json.as_ref().and_then(send_status).map_or(Kind::Hidden, |(target, delivery)| Kind::Status { target, delivery });
                    m.disappear = json.as_ref().and_then(|c| c.get("disappearing_timer")).map_or(0, |t| disappear::timer(t).0);
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
        TimelineItemContent::OtherState(state) if state.content().event_type().to_string() == disappear::TIMER => {
            let actor = (!viewer.service.contains(event.sender())).then_some(if m.own { t!("You") } else { m.sender.as_str() });
            let prev = event.latest_json().and_then(|raw| raw.get_field::<serde_json::Value>("unsigned").ok().flatten());
            let prev = prev.as_ref().and_then(|unsigned| unsigned.get("prev_content"));
            match json.as_ref().filter(|_| !flag("com.beeper.exclude_from_timeline")).and_then(|c| disappear::notice(c, prev, actor, m.own)) {
                Some(line) => (m.kind, m.body) = (Kind::Notice, line),
                None => m.kind = Kind::Hidden,
            }
        }
        TimelineItemContent::OtherState(state) => {
            let actor = (!viewer.service.contains(event.sender())).then_some(if m.own { t!("You") } else { m.sender.as_str() });
            match state_line(state.content(), actor, m.own) {
                Some(line) => (m.kind, m.body) = (Kind::Notice, line),
                None => m.kind = Kind::Hidden,
            }
        }
        _ => m.kind = Kind::Hidden,
    }
    if let (Some(media), Some(json)) = (&mut m.media, &json) {
        media.looped = super::video::looped(json);
        media.once = !m.own && json.get("com.beeper.view_limited").is_some_and(|v| v.is_object());
    }
    m.delivery = match event.send_state() {
        Some(EventSendState::NotSentYet { .. }) => Delivery::Sending,
        Some(EventSendState::SendingFailed { error, .. }) => Delivery::Failed(clip(&plain(error))),
        // On another network it has arrived once the bridge says so (a `Status` row), unless that
        // network's own app sent it, which the bridge marks.
        _ if m.own && viewer.bridged && !json.as_ref().is_some_and(|c| c.get("fi.mau.double_puppet_source").is_some()) => Delivery::Sending,
        _ => Delivery::Sent,
    };
    m.seen = event
        .read_receipts()
        .iter()
        .filter(|(user, _)| **user != viewer.own && !viewer.service.contains(*user))
        .take(MAX_SEEN)
        .map(|(user, receipt)| (user.clone(), receipt.ts.map_or(0, |ts| u64::from(ts.0))))
        .collect();
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
            format(m, message.msgtype());
            (m.mentions, m.mentioned, m.to_me) = marks(&m.body, message.msgtype(), message.mentions(), own);
            m.mentioned &= !m.own;
            m.edited = message.is_edited();
        }
        MsgLikeKind::Sticker(sent) => m.media = Some(sticker(sent.content())),
        MsgLikeKind::Poll(state) => m.poll = Some(poll(state)),
        MsgLikeKind::Redacted => (m.body, m.deleted) = (deleted().to_owned(), true),
        MsgLikeKind::UnableToDecrypt(_) => m.body = utd().to_owned(),
        MsgLikeKind::Other(_) | MsgLikeKind::LiveLocation(_) => m.kind = Kind::Hidden,
    }
}

fn quote(reply: &InReplyToDetails, own: &UserId) -> Quote {
    let id = reply.event_id.to_string();
    match &reply.event {
        TimelineDetails::Ready(quoted) => {
            let mut m = Msg::bare(id, Kind::Message, quoted.sender.clone());
            m.own = quoted.sender == own;
            m.sender = name(&quoted.sender_profile, &quoted.sender);
            if let TimelineItemContent::MsgLike(like) = &quoted.content {
                fill(&mut m, &like.kind, own);
            }
            m.quote()
        }
        TimelineDetails::Pending => Quote::unknown(id, QuoteState::Loading),
        TimelineDetails::Unavailable | TimelineDetails::Error(_) => Quote::unknown(id, QuoteState::Missing),
    }
}

fn poll(state: &PollState) -> Poll {
    let counted = state.results();
    let answers = counted.answers.into_iter().map(|answer| (answer.id, answer.text));
    Poll::counted(&counted.question, answers, counted.max_selections, counted.end_time.map(|t| u64::from(t.0)), counted.votes)
}

fn membership(m: &mut Msg, change: &RoomMembershipChange, viewer: &Viewer) {
    let me = change.user_id() == viewer.own;
    let subject = if me { t!("You").to_owned() } else { change.display_name().unwrap_or_else(|| change.user_id().localpart().to_owned()) };
    // Something a bot did was done by someone on the other network, who is not named.
    let actor = (!viewer.service.contains(&m.from) && m.from != change.user_id()).then_some(if m.own { t!("You") } else { m.sender.as_str() });
    match change.change() {
        Some(MembershipChange::Joined | MembershipChange::InvitationAccepted) => (m.kind, m.sender) = (Kind::Joined, subject),
        Some(change) => match membership_line(change, actor.map(|a| (a, m.own)), &subject, me) {
            Some(line) => (m.kind, m.body) = (Kind::Notice, line),
            None => m.kind = Kind::Hidden,
        },
        None => m.kind = Kind::Hidden,
    }
}

/// A change of someone's membership as a line; `None` for the ones that get no line. `actor` is
/// whoever did it to `subject`, when that is someone else and known, and whether that is the user.
/// Each case is a whole sentence, so a translation can inflect it.
fn membership_line(change: MembershipChange, actor: Option<(&str, bool)>, subject: &str, me: bool) -> Option<String> {
    use MembershipChange::{Banned, Invited, Kicked, KickedAndBanned, Left};
    Some(match (change, actor) {
        (Left, _) if me => t!("You left").to_owned(),
        (Left, _) => t!("{subject} left", subject = subject),
        (Kicked | KickedAndBanned | Banned, Some((_, true))) => t!("You removed {object}", object = subject),
        (Kicked | KickedAndBanned | Banned, Some((actor, _))) if me => t!("{actor} removed you", actor = actor),
        (Kicked | KickedAndBanned | Banned, Some((actor, _))) => t!("{actor} removed {object}", actor = actor, object = subject),
        (Kicked | KickedAndBanned | Banned, None) if me => t!("You were removed").to_owned(),
        (Kicked | KickedAndBanned | Banned, None) => t!("{subject} was removed", subject = subject),
        (Invited, Some((_, true))) => t!("You invited {object}", object = subject),
        (Invited, Some((actor, _))) if me => t!("{actor} invited you", actor = actor),
        (Invited, Some((actor, _))) => t!("{actor} invited {object}", actor = actor, object = subject),
        _ => return None,
    })
}

/// A change to the chat itself as a line.
fn state_line(change: &AnyOtherStateEventContentChange, actor: Option<&str>, own: bool) -> Option<String> {
    let actor = actor.unwrap_or_default();
    Some(match (change, actor.is_empty(), own) {
        (AnyOtherStateEventContentChange::RoomName(StateEventContentChange::Original { content, .. }), anyone, own) if !content.name.is_empty() => {
            let name = clip(&content.name);
            match (anyone, own) {
                (true, _) => t!("The name was changed to “{name}”", name = name),
                (false, true) => t!("You changed the name to “{name}”", name = name),
                (false, false) => t!("{actor} changed the name to “{name}”", actor = actor, name = name),
            }
        }
        (AnyOtherStateEventContentChange::RoomAvatar(_), true, _) => t!("The picture was changed").to_owned(),
        (AnyOtherStateEventContentChange::RoomAvatar(_), false, true) => t!("You changed the picture").to_owned(),
        (AnyOtherStateEventContentChange::RoomAvatar(_), false, false) => t!("{actor} changed the picture", actor = actor),
        (AnyOtherStateEventContentChange::RoomTopic(_), true, _) => t!("The description was changed").to_owned(),
        (AnyOtherStateEventContentChange::RoomTopic(_), false, true) => t!("You changed the description").to_owned(),
        (AnyOtherStateEventContentChange::RoomTopic(_), false, false) => t!("{actor} changed the description", actor = actor),
        _ => return None,
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

    /// Whether a message-like event of `kind` with `content` gets a row in a group chat.
    fn shows(kind: &str, content: serde_json::Value) -> bool {
        let event = json!({ "type": kind, "event_id": "$e", "sender": "@b:x", "origin_server_ts": 1, "content": content });
        let event: AnySyncTimelineEvent = serde_json::from_value(event).unwrap();
        let viewer = Viewer { own: "@a:x".try_into().unwrap(), service: BTreeSet::new(), bridged: false };
        shown(&event, &RoomVersionRules::V11, false, false, &viewer)
    }

    #[test]
    fn unknown_message_types_show_their_body() {
        assert!(shows("m.room.message", json!({ "msgtype": "com.example.custom", "body": "fallback" })));
        assert!(!shows("m.room.message", json!({ "msgtype": "com.example.custom", "body": " " })));
        let edit = json!({ "msgtype": "com.example.custom", "body": "* x", "m.new_content": { "msgtype": "com.example.custom", "body": "x" },
            "m.relates_to": { "rel_type": "m.replace", "event_id": "$o" } });
        assert!(!shows("m.room.message", edit));
        assert!(shows("m.room.message", json!({ "msgtype": "m.text", "body": "hi" })));
        assert!(shows("m.poll.start", json!({ "m.text": [{ "body": "Q?" }], "m.poll": { "kind": "m.disclosed", "max_selections": 1,
            "question": { "m.text": [{ "body": "Q?" }] }, "answers": [{ "m.id": "a", "m.text": [{ "body": "A" }] }] } })));
        assert!(!shows("m.poll.response", json!({ "m.selections": ["a"], "m.relates_to": { "rel_type": "m.reference", "event_id": "$o" } })));
        assert!(!shows("m.reaction", json!({ "m.relates_to": { "rel_type": "m.annotation", "event_id": "$o", "key": "x" } })));
    }
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
        let line = |change, actor: Option<&str>, subject, me| membership_line(change, actor.map(|a| (a, a == "You")), subject, me);
        assert_eq!(line(MembershipChange::Left, None, "Ann", false).as_deref(), Some("Ann left"));
        assert_eq!(line(MembershipChange::Left, None, "You", true).as_deref(), Some("You left"));
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
