//! Leaving and deleting chats, and starting new ones: on Matrix itself, and through a bridge's
//! provisioning API (mautrix bridgev2, `bridgev2/matrix/provisioning.go`).

use std::{
    collections::HashSet,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use matrix_sdk::{
    Client, Room, RoomState,
    deserialized_responses::RawAnySyncOrStrippedState,
    reqwest::{Method, Url},
    ruma::{
        OwnedRoomId, OwnedUserId, RoomId, UserId,
        serde::Raw,
        api::client::room::create_room,
        events::{InitialStateEvent, StateEventType, room::encryption::RoomEncryptionEventContent},
    },
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Ev, Rooms};
use crate::bridges::Api;
use crate::i18n::t;

/// Chats being left or deleted at once; each holds a task for at most `BRIDGE_DELETE_WAIT`.
const LEAVING_MAX: usize = 32;
/// How long a bridge is given to delete a chat's room before the user leaves it themselves.
const BRIDGE_DELETE_WAIT: Duration = Duration::from_secs(60);
/// How long a chat a bridge just made is waited for to reach this device.
const ROOM_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(1);
/// A bridge's contact list is cut here: a phone book bigger than this is searched, not listed.
pub const MAX_PEOPLE: usize = 5000;
/// Results from the homeserver's user directory.
const DIRECTORY_LIMIT: u64 = 20;
/// Creating a portal makes a room and can backfill; it takes longer than a plain call.
const CREATE_TIMEOUT: Duration = Duration::from_secs(90);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a new chat is made.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Matrix,
    /// A bridge, through one of the user's logins on it.
    Bridge { bridge: String, login: String },
}

/// Someone a chat can be started with.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    /// What the bridge or the homeserver takes to reach them: an MXID (a bridge's ghost or a
    /// Matrix user) or the network's own user ID.
    pub id: String,
    pub name: String,
    /// A phone number, username or MXID to tell people with one name apart.
    pub detail: String,
}

/// What a network lets a new chat be started with (`bridgev2.ProvisioningCapabilities`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Caps {
    pub phone: bool,
    pub username: bool,
    pub email: bool,
    pub contacts: bool,
    pub search: bool,
    pub create_dm: bool,
    pub group: Option<GroupCaps>,
}

/// `bridgev2.GroupTypeCapabilities` of the one group type offered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupCaps {
    pub kind: String,
    pub name: bool,
    pub name_required: bool,
    /// 0 is no limit.
    pub name_max: usize,
    /// Members besides the user.
    pub min_people: usize,
    pub max_people: usize,
}

impl Caps {
    pub fn matrix() -> Self {
        Caps {
            username: true,
            search: true,
            create_dm: true,
            group: Some(GroupCaps { name: true, name_max: 255, min_people: 1, ..Default::default() }),
            ..Default::default()
        }
    }
}

pub enum Ask {
    /// Whether the chat's bridge can delete it on the network.
    LeaveOptions(OwnedRoomId),
    /// `delete` sends `com.beeper.delete_chat` first.
    Leave { room: OwnedRoomId, delete: bool },
    /// Takes an invitation (joins) or a message request (`com.beeper.accept_message_request`).
    Accept(OwnedRoomId),
    /// Ignores who sent the invitation, or has the bridge block the person behind a message
    /// request (`com.beeper.block_user`); the chat is left either way.
    Block(OwnedRoomId),
    /// Joins the room that replaced this upgraded one.
    Upgrade(OwnedRoomId),
    /// The network's capabilities, then its contacts when it lists them.
    Open(Target),
    Find { target: Target, query: String },
    /// An identifier typed for a group: who it is.
    Resolve { target: Target, who: String },
    Start { target: Target, who: String },
    /// The user's note to self: the one there is, or a new one.
    Note,
    Group { target: Target, kind: String, name: String, people: Vec<String> },
}

pub enum Answer {
    LeaveOptions { room: OwnedRoomId, delete: bool },
    /// The invitation or message request was taken.
    Accepted(OwnedRoomId),
    Caps(Target, Caps),
    /// `query` empty: the contact list.
    People { target: Target, query: String, people: Vec<Person> },
    Resolved(Target, Person),
    /// The new chat, joined.
    Ready(OwnedRoomId),
    Failed(String),
}

static LEAVING: Mutex<Option<HashSet<OwnedRoomId>>> = Mutex::new(None);

/// The chat is on its way out: the sync loop must not put it back in the list.
pub(super) fn leaving(room: &RoomId) -> bool {
    LEAVING.lock().unwrap().as_ref().is_some_and(|set| set.contains(room))
}

fn mark(room: &RoomId) -> bool {
    let mut guard = LEAVING.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    set.len() < LEAVING_MAX && set.insert(room.to_owned())
}

fn unmark(room: &RoomId) {
    if let Some(set) = LEAVING.lock().unwrap().as_mut() {
        set.remove(room);
    }
}

pub(super) fn handle(client: Client, api: Option<Api>, rooms: Rooms, ev: mpsc::Sender<Ev>, ask: Ask) {
    tokio::spawn(async move {
        let answer = match ask {
            Ask::LeaveOptions(room) => {
                let delete = match client.get_room(&room) {
                    Some(r) => can_delete(&r).await,
                    None => false,
                };
                Answer::LeaveOptions { room, delete }
            }
            Ask::Leave { room, delete } => return leave(&client, &rooms, &ev, room, delete).await,
            Ask::Accept(room) => match accept(&client, &room).await {
                Ok(()) => Answer::Accepted(room),
                Err(e) => Answer::Failed(t!("Could not accept: {error}", error = super::plain(&format!("{e:#}")))),
            },
            Ask::Block(room) => {
                if let Err(e) = block(&client, &room).await {
                    let _ = ev.send(Ev::Error(t!("Could not block: {error}", error = super::plain(&format!("{e:#}"))))).await;
                    return;
                }
                return leave(&client, &rooms, &ev, room, false).await;
            }
            Ask::Upgrade(room) => outcome(successor(&client, &room).await),
            Ask::Open(target) => {
                let caps = match &target {
                    Target::Matrix => Ok(Caps::matrix()),
                    Target::Bridge { bridge, login } => match &api {
                        Some(api) => provision(api, bridge, login, Method::GET, &["capabilities"], None, CALL_TIMEOUT).await.map(|v| caps(&v)),
                        None => Err(anyhow!(t!("Networks are a Beeper feature"))),
                    },
                };
                match caps {
                    Ok(caps) => {
                        let contacts = caps.contacts;
                        let _ = ev.send(Ev::People(Answer::Caps(target.clone(), caps))).await;
                        if !contacts {
                            return;
                        }
                        match find(&client, api.as_ref(), &target, "").await {
                            Ok(people) => Answer::People { target, query: String::new(), people },
                            Err(e) => Answer::Failed(t!("Could not load contacts: {error}", error = super::plain(&format!("{e:#}")))),
                        }
                    }
                    Err(e) => Answer::Failed(t!("Could not reach the bridge: {error}", error = super::plain(&format!("{e:#}")))),
                }
            }
            Ask::Find { target, query } => match find(&client, api.as_ref(), &target, &query).await {
                Ok(people) => Answer::People { target, query, people },
                Err(e) => Answer::Failed(super::plain(&format!("{e:#}"))),
            },
            Ask::Resolve { target, who } => match resolve(api.as_ref(), &target, &who).await {
                Ok(person) => Answer::Resolved(target, person),
                Err(e) => Answer::Failed(super::plain(&format!("{e:#}"))),
            },
            Ask::Start { target, who } => outcome(start(&client, api.as_ref(), &target, &who).await),
            Ask::Note => outcome(note(&client).await),
            Ask::Group { target, kind, name, people } => outcome(group(&client, api.as_ref(), &target, &kind, &name, people).await),
        };
        let _ = ev.send(Ev::People(answer)).await;
    });
}

fn outcome(made: Result<OwnedRoomId>) -> Answer {
    match made {
        Ok(room) => Answer::Ready(room),
        Err(e) => Answer::Failed(super::plain(&format!("{e:#}"))),
    }
}

/// The bridge says in `com.beeper.room_features` (any state key: bridgev2 keys it by bridge) that
/// it deletes the chat on the network when sent `com.beeper.delete_chat`.
async fn can_delete(room: &Room) -> bool {
    let events = room.get_state_events(StateEventType::from("com.beeper.room_features")).await.unwrap_or_default();
    events.iter().any(|e| {
        let RawAnySyncOrStrippedState::Sync(raw) = e else { return false };
        let content: Option<Value> = raw.get_field("content").ok().flatten();
        content.and_then(|c| c.get("delete_chat")?.as_bool()).unwrap_or(false)
    })
}

/// The chat leaves the list at once; it comes back with an error when leaving fails.
async fn leave(client: &Client, rooms: &Rooms, ev: &mpsc::Sender<Ev>, id: OwnedRoomId, delete: bool) {
    let Some(room) = client.get_room(&id) else {
        let _ = ev.send(Ev::RoomGone(id)).await;
        return;
    };
    if !mark(&id) {
        let _ = ev.send(Ev::Error(t!("Already leaving this chat").to_owned())).await;
        return;
    }
    let summary = rooms.lock().unwrap().remove(&id);
    let _ = ev.send(Ev::RoomGone(id.clone())).await;
    let result = if delete { delete_chat(&room).await } else { leave_and_forget(&room).await };
    unmark(&id);
    if let Err(e) = result {
        if let Some(summary) = summary {
            rooms.lock().unwrap().insert(id, summary.clone());
            let _ = ev.send(Ev::Rooms(vec![summary])).await;
        }
        let _ = ev.send(Ev::Error(t!("Could not leave the chat: {error}", error = super::plain(&format!("{e:#}"))))).await;
    }
}

async fn accept(client: &Client, id: &RoomId) -> Result<()> {
    let room = client.get_room(id).context(t!("Unknown chat"))?;
    if room.state() == RoomState::Invited {
        room.join().await?;
    } else {
        room.send_raw("com.beeper.accept_message_request", json!({})).await?;
    }
    Ok(())
}

async fn block(client: &Client, id: &RoomId) -> Result<()> {
    let room = client.get_room(id).context(t!("Unknown chat"))?;
    if room.state() == RoomState::Invited {
        let invite = room.invite_details().await?;
        client.account().ignore_user(&invite.inviter_id).await?;
    } else {
        room.send_raw("com.beeper.block_user", json!({ "block": true })).await?;
    }
    Ok(())
}

/// The room an upgrade replaced this one with, joined.
async fn successor(client: &Client, id: &RoomId) -> Result<OwnedRoomId> {
    let room = client.get_room(id).context(t!("Unknown chat"))?;
    let next = room.successor_room().context(t!("Unknown chat"))?.room_id;
    if client.get_room(&next).is_none_or(|r| r.state() != RoomState::Joined) {
        client.join_room_by_id(&next).await?;
    }
    Ok(next)
}

async fn leave_and_forget(room: &Room) -> Result<()> {
    if matches!(room.state(), RoomState::Joined | RoomState::Invited | RoomState::Knocked) {
        room.leave().await?;
    }
    // Left already; forgetting only keeps it out of the server's list of old rooms.
    if let Err(e) = room.forget().await {
        log::debug!("forget {}: {e}", room.room_id());
    }
    Ok(())
}

/// Asks the bridge to delete the chat on the network, then waits for it to delete the room.
/// The bridges leave a group only while the user is still in the room (mautrix-whatsapp,
/// -signal and -meta `HandleMatrixDeleteChat` check the user's membership first), so the user
/// leaves only once the bridge had its time.
async fn delete_chat(room: &Room) -> Result<()> {
    room.send_raw("com.beeper.delete_chat", json!({})).await?;
    let deadline = Instant::now() + BRIDGE_DELETE_WAIT;
    while room.state() == RoomState::Joined && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    // The chat is deleted on the network either way; what remains is only this room.
    if let Err(e) = leave_and_forget(room).await {
        log::debug!("leave after delete {}: {e:#}", room.room_id());
    }
    Ok(())
}

/// A provisioning call for one login: `/_matrix/provision/v3/<segments>` behind Beeper's bridge
/// proxy, each segment percent-encoded.
async fn provision(api: &Api, bridge: &str, login: &str, method: Method, segments: &[&str], body: Option<Value>, timeout: Duration) -> Result<Value> {
    let base = format!("{}/_matrix/client/unstable/com.beeper.bridge/{bridge}/_matrix/provision/v3", api.homeserver.trim_end_matches('/'));
    let mut url = Url::parse(&base)?;
    url.path_segments_mut().map_err(|()| anyhow!("bad homeserver URL"))?.extend(segments);
    url.query_pairs_mut().append_pair("user_id", &api.user_id).append_pair("login_id", login);
    api.send(method, url.into(), body, timeout).await
}

/// Tells the chat's bridge that the user opened view-once media, so that it is used up on the
/// network (bridgev2 `POST /v3/view_limited_media/{event}`, body `com.beeper.view_limited`).
pub(super) async fn report_once(api: &Api, room: &Room, event: &str) -> Result<()> {
    let protocol = super::protocol_id(room).await.context("the chat is not bridged")?;
    let networks = api.networks().await?;
    let network = networks.iter().find(|n| n.bridge == protocol).or_else(|| networks.iter().find(|n| n.bridge.starts_with(&protocol))).context("no bridge for the chat")?;
    let login = network.logins.iter().find(|l| l.state == "CONNECTED").or(network.logins.first()).context("no login on the bridge")?;
    let body = json!({ "com.beeper.view_limited": { "type": "count", "count": 1 } });
    provision(api, &network.bridge, &login.id, Method::POST, &["view_limited_media", event], Some(body), CALL_TIMEOUT).await.map(drop)
}

fn caps(v: &Value) -> Caps {
    let flag = |path: &str| v.pointer(path).and_then(Value::as_bool).unwrap_or(false);
    let size = |g: &Value, path: &str| g.pointer(path).and_then(Value::as_u64).unwrap_or(0) as usize;
    // shortcut: the first group type by name is offered when a network has several; let the user
    // choose when one does (none of WhatsApp, Signal, Meta does).
    let group = v.get("group_creation").and_then(Value::as_object).and_then(|types| types.iter().min_by_key(|(kind, _)| kind.as_str())).map(|(kind, g)| {
        let field = |path: &str| g.pointer(path).and_then(Value::as_bool).unwrap_or(false);
        GroupCaps {
            kind: kind.clone(),
            name: field("/name/allowed"),
            name_required: field("/name/required"),
            name_max: size(g, "/name/max_length"),
            min_people: size(g, "/participants/min_length"),
            max_people: size(g, "/participants/max_length"),
        }
    });
    Caps {
        phone: flag("/resolve_identifier/lookup_phone") || flag("/resolve_identifier/any_phone"),
        username: flag("/resolve_identifier/lookup_username"),
        email: flag("/resolve_identifier/lookup_email"),
        contacts: flag("/resolve_identifier/contact_list"),
        search: flag("/resolve_identifier/search"),
        create_dm: flag("/resolve_identifier/create_dm"),
        group,
    }
}

/// One `RespResolveIdentifier`.
fn person(v: &Value) -> Option<Person> {
    let text = |key: &str| v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
    let network_id = text("id")?;
    let detail = v
        .get("identifiers")
        .and_then(Value::as_array)
        .and_then(|ids| ids.iter().filter_map(Value::as_str).next())
        .map(|id| id.strip_prefix("tel:").or_else(|| id.strip_prefix("mailto:")).unwrap_or(id).to_owned())
        .unwrap_or_default();
    Some(Person {
        // A ghost's MXID: the bridge turns it back into the network ID (`provisionutil.ResolveIdentifier`).
        id: text("mxid").unwrap_or(network_id).to_owned(),
        name: text("name").map(str::to_owned).unwrap_or_else(|| if detail.is_empty() { network_id.to_owned() } else { detail.clone() }),
        detail,
    })
}

fn people(v: &Value, key: &str) -> Vec<Person> {
    v.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(person).take(MAX_PEOPLE).collect()
}

fn bridge_api(api: Option<&Api>) -> Result<&Api> {
    api.ok_or_else(|| anyhow!(t!("Networks are a Beeper feature")))
}

/// `query` empty: the contact list.
async fn find(client: &Client, api: Option<&Api>, target: &Target, query: &str) -> Result<Vec<Person>> {
    match target {
        Target::Matrix => {
            let found = client.search_users(query, DIRECTORY_LIMIT).await?;
            let own = client.user_id();
            Ok(found
                .results
                .into_iter()
                .filter(|u| Some(&*u.user_id) != own)
                .map(|u| Person {
                    name: u.display_name.filter(|n| !n.is_empty()).unwrap_or_else(|| u.user_id.localpart().to_owned()),
                    detail: u.user_id.to_string(),
                    id: u.user_id.to_string(),
                })
                .collect())
        }
        Target::Bridge { bridge, login } => {
            let api = bridge_api(api)?;
            if query.is_empty() {
                Ok(people(&provision(api, bridge, login, Method::GET, &["contacts"], None, CALL_TIMEOUT).await?, "contacts"))
            } else {
                Ok(people(&provision(api, bridge, login, Method::POST, &["search_users"], Some(json!({ "query": query })), CALL_TIMEOUT).await?, "results"))
            }
        }
    }
}

fn matrix_user(who: &str) -> Result<OwnedUserId> {
    UserId::parse(who.trim()).map_err(|_| anyhow!(t!("Enter a full Matrix ID, like @name:matrix.org")))
}

async fn resolve(api: Option<&Api>, target: &Target, who: &str) -> Result<Person> {
    match target {
        Target::Matrix => {
            let user = matrix_user(who)?;
            Ok(Person { id: user.to_string(), name: user.localpart().to_owned(), detail: user.to_string() })
        }
        Target::Bridge { bridge, login } => {
            let found = provision(bridge_api(api)?, bridge, login, Method::GET, &["resolve_identifier", who.trim()], None, CALL_TIMEOUT).await?;
            person(&found).context(t!("The bridge did not find anyone"))
        }
    }
}

/// A one-to-one chat: the one there is, or a new one.
async fn start(client: &Client, api: Option<&Api>, target: &Target, who: &str) -> Result<OwnedRoomId> {
    match target {
        Target::Matrix => {
            let user = matrix_user(who)?;
            if client.user_id() == Some(&*user) {
                bail!(t!("That is your own account"));
            }
            let room = match client.get_dm_room(&user) {
                Some(room) => room,
                None => client.create_dm(&user).await?,
            };
            Ok(room.room_id().to_owned())
        }
        Target::Bridge { bridge, login } => {
            // `create_dm` answers with the chat there is, when there is one (`provisionutil.ResolveIdentifier`).
            let made = provision(bridge_api(api)?, bridge, login, Method::POST, &["create_dm", who.trim()], Some(json!({})), CREATE_TIMEOUT).await?;
            let room = made.get("dm_room_mxid").and_then(Value::as_str).context(t!("The bridge made no chat"))?;
            arrived(client, RoomId::parse(room)?).await
        }
    }
}

/// Beeper's own note to self: a private encrypted room of one, named "Note to self" and marked by
/// an empty `com.beeper.rooms.note_to_self` state event (read from the owner's Beeper account).
async fn note(client: &Client) -> Result<OwnedRoomId> {
    for room in client.joined_rooms() {
        if super::is_note(&room).await {
            return Ok(room.room_id().to_owned());
        }
    }
    let mut request = create_room::v3::Request::new();
    request.name = Some("Note to self".to_owned());
    request.preset = Some(create_room::v3::RoomPreset::PrivateChat);
    let mark = Raw::new(&json!({ "type": super::NOTE_TO_SELF, "state_key": "", "content": {} }))?.cast_unchecked();
    request.initial_state = vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any(), mark];
    let room = client.create_room(request).await?;
    // The chat opens once its marker has come back in a sync: before that it opens as a group ("You
    // created the group"). A sync can bring the room before the marker, so `sync_up` is not enough.
    // Bounded: sync may be down.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !super::is_note(&room).await && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(room.room_id().to_owned())
}

async fn group(client: &Client, api: Option<&Api>, target: &Target, kind: &str, name: &str, people: Vec<String>) -> Result<OwnedRoomId> {
    let name = name.trim();
    match target {
        Target::Matrix => {
            let invite = people.iter().map(|p| matrix_user(p)).collect::<Result<Vec<_>>>()?;
            let mut request = create_room::v3::Request::new();
            request.name = (!name.is_empty()).then(|| name.to_owned());
            request.invite = invite;
            request.preset = Some(create_room::v3::RoomPreset::PrivateChat);
            request.initial_state = vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any()];
            Ok(client.create_room(request).await?.room_id().to_owned())
        }
        Target::Bridge { bridge, login } => {
            // `bridgev2.GroupCreateParams`; participants may be ghost MXIDs (`provisionutil.CreateGroup`).
            let mut body = json!({ "participants": people });
            if !name.is_empty() {
                body["name"] = json!({ "name": name });
            }
            let made = provision(bridge_api(api)?, bridge, login, Method::POST, &["create_group", kind], Some(body), CREATE_TIMEOUT).await?;
            let room = made.get("mxid").and_then(Value::as_str).filter(|r| !r.is_empty()).context(t!("The bridge made no chat"))?;
            arrived(client, RoomId::parse(room)?).await
        }
    }
}

/// Waits for a room a bridge made to reach this device, joining it when it is an invitation.
async fn arrived(client: &Client, id: OwnedRoomId) -> Result<OwnedRoomId> {
    let deadline = Instant::now() + ROOM_WAIT;
    loop {
        match client.get_room(&id).map(|r| (r.state(), r)) {
            Some((RoomState::Joined, _)) => return Ok(id),
            Some((RoomState::Invited, room)) => room.join().await?,
            _ if Instant::now() >= deadline => bail!(t!("The chat was made but has not reached this device yet")),
            _ => tokio::time::sleep(POLL / 2).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_capabilities_offer_phone_contacts_and_a_group() {
        // mautrix-whatsapp pkg/connector/capabilities.go, as bridgev2 serialises it.
        let v = json!({
            "resolve_identifier": { "create_dm": true, "lookup_phone": true, "lookup_email": false, "lookup_username": false,
                                    "any_phone": false, "contact_list": true, "search": false },
            "group_creation": { "group": { "type_description": "a group chat", "name": { "allowed": true, "max_length": 100 },
                                           "participants": { "allowed": true, "required": true, "min_length": 1 } } }
        });
        let c = caps(&v);
        assert!(c.phone && c.contacts && c.create_dm && !c.search && !c.username);
        let g = c.group.unwrap();
        assert_eq!((g.kind.as_str(), g.name, g.name_required, g.name_max, g.min_people, g.max_people), ("group", true, false, 100, 1, 0));
        assert_eq!(caps(&json!({})), Caps::default(), "an old bridge without the endpoint offers nothing");
    }

    #[test]
    fn a_contact_is_reached_by_its_ghost_and_shown_by_its_number() {
        let v = json!({ "contacts": [
            { "id": "4915112345", "name": "Ann", "identifiers": ["tel:+4915112345"], "mxid": "@whatsapp_4915112345:beeper.local" },
            { "id": "4915199999", "identifiers": ["tel:+4915199999"] },
            { "name": "no id" }
        ]});
        let got = people(&v, "contacts");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Person { id: "@whatsapp_4915112345:beeper.local".into(), name: "Ann".into(), detail: "+4915112345".into() });
        assert_eq!((got[1].id.as_str(), got[1].name.as_str()), ("4915199999", "+4915199999"));
    }
}
