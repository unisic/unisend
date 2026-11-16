//! What a chat's info screen shows: its details and members, and the media, links and files found
//! by reading its history backwards, within a fixed budget.

use std::collections::HashMap;

use matrix_sdk::{
    EncryptionState, Room, RoomMemberships,
    room::RoomMemberRole,
    ruma::{OwnedRoomId, OwnedUserId, UserId, events::room::power_levels::RoomPowerLevels},
};
use tokio::sync::mpsc;

use super::group::{self, Can};
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
    /// The user may remove them from the group.
    pub removable: bool,
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
    pub can: Can,
    /// The room's bridge as `(bridge, login)`, for starting a chat with a member through it.
    pub bridge: Option<(String, String)>,
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
        details.identifiers = identifiers_of(&room, peer).await;
    } else {
        let (can, levels) = group::can(&room).await;
        details.members = members(&room, levels.as_ref()).await;
        (details.can, details.members_loaded) = (can, true);
        details.bridge = group::bridge(&room).await;
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

pub(super) async fn identifiers_of(room: &Room, peer: &UserId) -> Vec<String> {
    match room.client().account().fetch_user_profile_of(peer).await {
        Ok(profile) => {
            let mut ids = identifiers(profile.get("com.beeper.bridge.identifiers"));
            ids.extend(facebook_id(peer).map(|id| format!("facebook:{id}")));
            ids
        }
        Err(e) => {
            log::debug!("profile of the other person: {e}");
            Vec::new()
        }
    }
}

/// How the other person in a one-to-one chat is known on its network; empty in a group.
pub(super) async fn peer(room: Room, ev: mpsc::Sender<Ev>) {
    let Some(peer) = other_person(&room).await else { return };
    let ids = identifiers_of(&room, &peer).await;
    let _ = ev.send(Ev::PeerIds(room.room_id().to_owned(), ids)).await;
}

/// The Facebook id in a Meta ghost's MXID, `@facebookgo_<id>:beeper.local` (the fixture in
/// backend/tests.rs; the bridge's identifiers carry none). Only Beeper's own bridge server counts.
fn facebook_id(ghost: &UserId) -> Option<&str> {
    let (name, id) = ghost.localpart().rsplit_once('_')?;
    (ghost.server_name() == "beeper.local" && name.starts_with("facebook") && !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// Whether `profile_link` can ever give a link in a chat on `network`, so a profile is worth fetching.
pub fn links_profiles(network: Option<&str>) -> bool {
    network.is_some_and(|n| ["instagram", "messenger", "facebook", "whatsapp", "telegram", "twitter", "bluesky"].iter().any(|k| n.to_ascii_lowercase().contains(k)) || n == "X")
}

/// The network app's own page for the other person, as `(app, https link)`: a link its Android app
/// claims (App Links) and a browser opens otherwise. Formats from the bridges: Instagram
/// `instagram:<username>` (meta igconnector chatinfo.go), WhatsApp `tel:+<digits>` (whatsapp
/// userinfo.go), Telegram `telegram:<username>` (telegram userinfo.go), X `twitter:<screen name>`
/// (twitter chatinfo.go), Bluesky `bluesky:<handle>` (bluesky chatinfo.go). Messenger ghosts carry
/// none, so `facebook:<id>` is made from the ghost's MXID (`facebook_id`); Signal usernames link only through an encrypted `#eu/` token; LinkedIn gives an opaque URN.
pub fn profile_link(network: Option<&str>, identifiers: &[String]) -> Option<(&'static str, String)> {
    // Identifiers come from the other side's profile, so only plain names go into a link.
    let plain = |s: &str, extra: &[char]| !s.is_empty() && s.len() <= 100 && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c));
    let whatsapp = network.is_some_and(|n| n.to_ascii_lowercase().contains("whatsapp"));
    let messenger = network.is_some_and(|n| ["messenger", "facebook"].iter().any(|k| n.to_ascii_lowercase().contains(k)));
    identifiers.iter().find_map(|id| {
        let (kind, value) = id.split_once(':')?;
        match kind {
            "instagram" if plain(value, &['.', '_']) => Some(("Instagram", format!("https://www.instagram.com/{value}/"))),
            "telegram" if plain(value, &['_']) => Some(("Telegram", format!("https://t.me/{value}"))),
            "twitter" if plain(value, &['_']) => Some(("X", format!("https://x.com/{value}"))),
            "bluesky" if plain(value, &['.', '-']) => Some(("Bluesky", format!("https://bsky.app/profile/{value}"))),
            "facebook" if messenger && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit()) && !value.is_empty() => Some(("Facebook", format!("https://www.facebook.com/profile.php?id={value}"))),
            "tel" if whatsapp => value.strip_prefix('+').filter(|d| plain(d, &[]) && d.bytes().all(|b| b.is_ascii_digit())).map(|d| ("WhatsApp", format!("https://wa.me/{d}"))),
            _ => None,
        }
    })
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
async fn members(room: &Room, levels: Option<&RoomPowerLevels>) -> Vec<Member> {
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
            removable: levels.is_some_and(|l| m.user_id() != room.own_user_id() && l.user_can_kick_user(room.own_user_id(), m.user_id())),
            role: match m.suggested_role_for_power_level() {
                _ if service.contains(m.user_id()) => Role::Service,
                RoomMemberRole::Creator => Role::Creator,
                RoomMemberRole::Administrator => Role::Admin,
                RoomMemberRole::Moderator => Role::Moderator,
                RoomMemberRole::User => Role::Member,
            },
        })
        .collect();
    sort_members(&mut out, room.own_user_id());
    out.truncate(MAX_MEMBERS);
    out
}

/// The user first, as Signal lists them, then by role and name.
fn sort_members(list: &mut [Member], own: &UserId) {
    list.sort_by_cached_key(|m| (m.id != own, m.role, m.name.to_lowercase()));
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
    fn identifiers_become_profile_links() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let link = |network, list: &[&str]| profile_link(Some(network), &ids(list));
        assert_eq!(link("Instagram", &["instagram:ann.b_1"]), Some(("Instagram", "https://www.instagram.com/ann.b_1/".into())));
        assert_eq!(link("WhatsApp", &["tel:+48123456789"]), Some(("WhatsApp", "https://wa.me/48123456789".into())));
        assert_eq!(link("Telegram", &["tel:+48123", "telegram:ann_b"]), Some(("Telegram", "https://t.me/ann_b".into())));
        assert_eq!(link("X", &["twitter:ann"]), Some(("X", "https://x.com/ann".into())));
        assert_eq!(link("Bluesky", &["did:plc:abc", "bluesky:ann.bsky.social"]), Some(("Bluesky", "https://bsky.app/profile/ann.bsky.social".into())));
        // A phone number is a WhatsApp chat only on WhatsApp; Signal and SMS have no such link.
        assert_eq!(link("Signal", &["tel:+48123", "signal:ann.01"]), None);
        assert_eq!(link("Telegram", &["tel:+48123"]), None);
        assert_eq!(link("Instagram", &["instagram:a/../b", "instagram:", "instagram:a?x=1"]), None);
        assert_eq!(link("WhatsApp", &["tel:48123", "tel:+48 123"]), None);
        assert_eq!(link("Facebook Messenger", &["facebook:100001"]), Some(("Facebook", "https://www.facebook.com/profile.php?id=100001".into())));
        assert_eq!(link("Instagram", &["facebook:100001"]), None);
        assert_eq!(link("Messenger", &["facebook:1a", "facebook:", "facebook:123456789012345678901"]), None);
        let ghost = |s: &str| facebook_id(<&UserId>::try_from(s).unwrap()).map(str::to_owned);
        assert_eq!(ghost("@facebookgo_100001:beeper.local").as_deref(), Some("100001"));
        assert_eq!(ghost("@facebookgo_100001:evil.org"), None);
        assert_eq!(ghost("@facebookgo_1x:beeper.local"), None);
        assert_eq!(ghost("@signal_100001:beeper.local"), None);
        assert_eq!(profile_link(None, &[]), None);
        assert!(links_profiles(Some("Instagram")) && links_profiles(Some("X")) && !links_profiles(Some("Signal")) && !links_profiles(None));
    }

    #[test]
    fn members_sort_by_user_then_role_then_name() {
        let m = |name: &str, role| Member { id: format!("@{}:x", name.to_lowercase()).try_into().unwrap(), name: name.into(), role, removable: false };
        let mut list = vec![m("bob", Role::Member), m("Zed", Role::Admin), m("Ann", Role::Member), m("bot", Role::Service), m("Cy", Role::Creator)];
        sort_members(&mut list, matrix_sdk::ruma::user_id!("@bob:x"));
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["bob", "Cy", "Zed", "bot", "Ann"]);
    }
}
