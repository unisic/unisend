//! What a chat's info screen shows: its details and members, and the media, links and files found
//! by reading its history backwards, within a fixed budget.

use std::collections::HashMap;

use matrix_sdk::{
    EncryptionState, Room, RoomMemberships,
    room::RoomMemberRole,
    ruma::{OwnedRoomId, OwnedUserId},
};
use tokio::sync::mpsc;

use super::{Block, Ev, Msg, link_preview, messages, other_person, to_msgs};

/// Events read back through the history at most, and messages worth listing kept at most: the
/// screen keeps 200 of each kind and there are three kinds.
pub const MAX_SCAN: usize = 2000;
const MAX_FOUND: usize = 600;
/// Events asked for per history request.
const SCAN_PAGE: u16 = 100;
/// Members listed at most; a group with more says how many there are.
pub const MAX_MEMBERS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Creator,
    Admin,
    Moderator,
    /// A bridge's bot or another service the room names in `m.member_hints`.
    Service,
    Member,
}

#[derive(Clone)]
pub struct Member {
    pub id: OwnedUserId,
    pub name: String,
    pub role: Role,
}

#[derive(Clone, Default)]
pub struct Details {
    pub topic: Option<String>,
    /// `None` while the server has not said.
    pub encrypted: Option<bool>,
    /// The other person in a one-to-one chat.
    pub peer: Option<OwnedUserId>,
    /// How the network knows the other person (`tel:+48...`, `signal:name`), from the bridge's
    /// profile fields; empty when it does not say.
    pub identifiers: Vec<String>,
    /// Sorted by role, then name; at most `MAX_MEMBERS`. Empty until loaded.
    pub members: Vec<Member>,
    pub joined: u64,
    pub members_loaded: bool,
}

/// Tells the UI the chat's details, then what its history holds, page by page. Runs until the
/// history or the budget ends; the caller aborts it when the screen closes.
pub(super) async fn run(room: Room, ev: mpsc::Sender<Ev>) {
    let id = room.room_id().to_owned();
    let peer = other_person(&room).await;
    let mut details = Details {
        topic: room.topic().filter(|t| !t.trim().is_empty()),
        encrypted: encrypted(room.encryption_state()),
        peer: peer.clone(),
        joined: room.joined_members_count(),
        ..Details::default()
    };
    if ev.send(Ev::Info(id.clone(), Box::new(details.clone()))).await.is_err() {
        return;
    }
    if details.encrypted.is_none() {
        details.encrypted = room.latest_encryption_state().await.ok().and_then(encrypted);
    }
    if let Some(peer) = &peer {
        details.identifiers = match room.client().account().fetch_user_profile_of(peer).await {
            Ok(profile) => identifiers(profile.get("com.beeper.bridge.identifiers")),
            Err(e) => {
                log::debug!("profile of the other person: {e}");
                Vec::new()
            }
        };
    } else {
        details.members = members(&room).await;
        details.members_loaded = true;
    }
    if ev.send(Ev::Info(id.clone(), Box::new(details))).await.is_err() {
        return;
    }
    scan(&room, &id, &ev).await;
}

fn encrypted(state: EncryptionState) -> Option<bool> {
    match state {
        EncryptionState::Unknown => None,
        state => Some(state.is_encrypted()),
    }
}

/// The bridge's identifiers, phone numbers first.
fn identifiers(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut out: Vec<String> = value
        .and_then(|v| v.as_array())
        .map(|list| list.iter().filter_map(|v| v.as_str()).filter(|s| !s.is_empty()).take(4).map(ToOwned::to_owned).collect())
        .unwrap_or_default();
    out.sort_by_key(|id| !id.starts_with("tel:"));
    out
}

/// Joined members with their roles. Fetches the list when lazy loading left it incomplete.
async fn members(room: &Room) -> Vec<Member> {
    let list = match room.members(RoomMemberships::JOIN).await {
        Ok(list) => list,
        Err(e) => {
            log::debug!("members of {}: {e}", room.room_id());
            room.members_no_sync(RoomMemberships::JOIN).await.unwrap_or_default()
        }
    };
    let service = room.service_members().unwrap_or_default();
    let mut out: Vec<Member> = list
        .iter()
        .map(|m| Member {
            id: m.user_id().to_owned(),
            name: m.name().to_owned(),
            role: match m.suggested_role_for_power_level() {
                _ if service.contains(m.user_id()) => Role::Service,
                RoomMemberRole::Creator => Role::Creator,
                RoomMemberRole::Administrator => Role::Admin,
                RoomMemberRole::Moderator => Role::Moderator,
                RoomMemberRole::User => Role::Member,
            },
        })
        .collect();
    sort_members(&mut out);
    out.truncate(MAX_MEMBERS);
    out
}

fn sort_members(list: &mut [Member]) {
    list.sort_by_cached_key(|m| (m.role, m.name.to_lowercase()));
}

/// Reads the history backwards and passes on the messages that carry media, a file or a link.
async fn scan(room: &Room, id: &OwnedRoomId, ev: &mpsc::Sender<Ev>) {
    let (mut from, mut scanned, mut found) = (None::<String>, 0, 0);
    loop {
        let page = match messages(room, from.as_deref(), SCAN_PAGE).await {
            Ok(page) => page,
            Err(e) => {
                log::debug!("info history of {id}: {e:#}");
                let _ = ev.send(Ev::InfoItems { room: id.clone(), items: Vec::new(), scanned, done: true }).await;
                return;
            }
        };
        scanned += page.chunk.len();
        let previews: HashMap<String, serde_json::Value> = page
            .chunk
            .iter()
            .filter_map(|e| Some((e.event_id()?.to_string(), e.raw().get_field::<serde_json::Value>("content").ok()??)))
            .filter(|(_, content)| content.get("com.beeper.linkpreviews").is_some())
            .collect();
        let (msgs, _) = to_msgs(room, &page.chunk).await;
        let items: Vec<Msg> = msgs
            .into_iter()
            .map(|mut m| {
                m.link = previews.get(&m.id).and_then(link_preview);
                m
            })
            .filter(worth_listing)
            .take(MAX_FOUND - found)
            .collect();
        found += items.len();
        from = page.end;
        let done = from.is_none() || scanned >= MAX_SCAN || found >= MAX_FOUND;
        if ev.send(Ev::InfoItems { room: id.clone(), items, scanned, done }).await.is_err() || done {
            return;
        }
    }
}

/// Carries something the screen lists. Link detection itself is the screen's; this only rules
/// out messages that cannot hold one.
fn worth_listing(m: &Msg) -> bool {
    fn linked(blocks: &[Block]) -> bool {
        blocks.iter().any(|b| match b {
            Block::Para(runs) => runs.iter().any(|r| r.href.as_deref().is_some_and(|h| h.starts_with("http") && !h.contains("matrix.to/#/"))),
            Block::Quote(inner) => linked(inner),
            Block::List { items, .. } => items.iter().any(|item| linked(item)),
            Block::Code(..) => false,
        })
    }
    !m.deleted && (m.media.is_some() || m.link.is_some() || m.body.contains("http") || m.rich.as_ref().is_some_and(|r| linked(&r.blocks)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_numbers_come_first_and_junk_is_dropped() {
        let v = serde_json::json!(["signal:ann.01", "tel:+48123", 5, ""]);
        assert_eq!(identifiers(Some(&v)), ["tel:+48123", "signal:ann.01"]);
        assert!(identifiers(None).is_empty());
        assert!(identifiers(Some(&serde_json::json!("tel:+1"))).is_empty());
    }

    #[test]
    fn members_sort_by_role_then_name() {
        let m = |name: &str, role| Member { id: format!("@{}:x", name.to_lowercase()).try_into().unwrap(), name: name.into(), role };
        let mut list = vec![m("bob", Role::Member), m("Zed", Role::Admin), m("Ann", Role::Member), m("bot", Role::Service), m("Cy", Role::Creator)];
        sort_members(&mut list);
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Cy", "Zed", "bot", "Ann", "bob"]);
    }
}
