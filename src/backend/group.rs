//! Changing a group (name, picture, description, members) and the user's own profile. A group is
//! changed where the room's power levels let the user and, in a bridged room, where the bridge's
//! `com.beeper.room_features` says it carries the change to the network (`state` by event type,
//! `member_actions` by action; mautrix-go `event/capabilities.go`). The bridge then makes the
//! change on the network from the Matrix state event, as for any other client.
//! The group link is the room's `matrix.to` permalink with the `knock` join rule: whoever has it
//! asks to join and a member who may invite lets them in, as Signal's link with "Approve new
//! members" on. No bridge takes `m.room.join_rules` from Matrix (mautrix-go bridgev2 only writes
//! it), so a bridged group has no link unless its features name that state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use matrix_sdk::{
    Client, Room, RoomMemberships,
    ruma::{
        OwnedUserId, UserId,
        events::{StateEventType, room::join_rules::JoinRule},
    },
};
use serde_json::Value;

use super::{caption, disappear, features, other_person};
use crate::i18n::t;

/// People offered when adding members: those the user has a one-to-one chat with.
pub const MAX_CANDIDATES: usize = 500;
/// Longest group name and description sent; servers take more, networks often less.
pub const NAME_MAX: usize = 100;
pub const TOPIC_MAX: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Name(String),
    Topic(String),
    Photo(PathBuf),
    Invite(OwnedUserId),
    Remove(OwnedUserId),
    /// The group link on (`knock`) or off (`invite`).
    Link(bool),
}

/// How someone who was not invited gets into a group.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Join {
    #[default]
    Invite,
    /// They ask with the link and a member lets them in.
    Knock,
    /// Anyone who has the link joins.
    Public,
    /// A rule this app does not offer (`restricted` to a space's members, a custom one).
    Other,
}

pub(super) fn join(room: &Room) -> Join {
    match room.join_rule() {
        None | Some(JoinRule::Invite | JoinRule::Private) => Join::Invite,
        Some(JoinRule::Knock) => Join::Knock,
        Some(JoinRule::Public) => Join::Public,
        Some(_) => Join::Other,
    }
}

pub enum Mine {
    Name(String),
    Photo(PathBuf),
}

/// What the user may change in a group.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Can {
    pub name: bool,
    pub photo: bool,
    pub topic: bool,
    pub invite: bool,
    /// Removing someone at all; `Member::removable` says whom.
    pub remove: bool,
    /// Turning the group link on and off: the join rule is the user's to change, the room's version
    /// has knocking (7 and later) and no bridge stands in the way.
    pub link: bool,
}

impl Can {
    pub fn edit(&self) -> bool {
        self.name || self.photo || self.topic
    }
}

/// The user's display name and picture, as the homeserver keeps them.
#[derive(Clone, Default)]
pub struct Profile {
    pub name: String,
    pub picture: Option<Vec<u8>>,
}

/// Whether the bridge's features let a change through. Without the event the room is plain
/// Matrix and takes everything; with it, a change it does not name is refused by the bridge.
fn allows(features: &[Value], section: &str, key: &str) -> bool {
    if features.is_empty() {
        return true;
    }
    features.iter().any(|f| {
        let level = f.get(section).and_then(|s| s.get(key));
        let level = if section == "state" { level.and_then(|l| l.get("level")) } else { level };
        level.and_then(Value::as_i64).is_some_and(|l| l >= 1)
    })
}

/// What the user may change here, and who of `members` they may remove.
pub(super) async fn can(room: &Room) -> (Can, Option<matrix_sdk::ruma::events::room::power_levels::RoomPowerLevels>) {
    let feats = disappear::state(room, features::KIND).await;
    let levels = match room.power_levels().await {
        Ok(levels) => levels,
        Err(e) => {
            log::debug!("power levels: {e}");
            return (Can::default(), None);
        }
    };
    let own = room.own_user_id();
    let state = |kind: StateEventType| levels.user_can_send_state(own, kind.clone()) && allows(&feats, "state", &kind.to_string());
    let can = Can {
        name: state(StateEventType::RoomName),
        photo: state(StateEventType::RoomAvatar),
        topic: state(StateEventType::RoomTopic),
        invite: levels.user_can_invite(own) && allows(&feats, "member_actions", "invite"),
        remove: levels.user_can_kick(own) && allows(&feats, "member_actions", "kick"),
        link: state(StateEventType::RoomJoinRules) && room.version().and_then(|v| v.rules()).is_some_and(|r| r.authorization.knocking),
    };
    (can, Some(levels))
}

/// The bridge of a bridged room, as `(bridge, login)` for its provisioning API: the bot is
/// `@<bridge>bot:<server>` (bbctl's `bot_username: {{ .BridgeName }}bot`), the login is the
/// portal's receiver, or empty for the bridge's default login.
pub(super) async fn bridge(room: &Room) -> Option<(String, String)> {
    disappear::state(room, "m.bridge").await.iter().find_map(|content| {
        let bot = UserId::parse(content.get("bridgebot")?.as_str()?).ok()?;
        let name = bot.localpart().strip_suffix("bot").filter(|n| !n.is_empty())?.to_owned();
        let login = content.pointer("/channel/fi.mau.receiver").and_then(Value::as_str).unwrap_or_default().to_owned();
        Some((name, login))
    })
}

pub(super) async fn change(room: &Room, change: &Change) -> Result<()> {
    match change {
        Change::Name(name) => {
            room.set_name(clip(name, NAME_MAX)).await?;
        }
        Change::Topic(topic) => {
            room.set_room_topic(&clip(topic, TOPIC_MAX)).await?;
        }
        Change::Photo(path) => {
            let (mime, bytes) = photo(path).await?;
            room.upload_avatar(&mime, bytes, None).await?;
        }
        Change::Invite(user) => room.invite_user_by_id(user).await?,
        Change::Remove(user) => room.kick_user(user, None).await?,
        Change::Link(on) => room.privacy_settings().update_join_rule(if *on { JoinRule::Knock } else { JoinRule::Invite }).await?,
    }
    Ok(())
}

fn clip(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// A picture to upload as an avatar: upright, at most `caption::SEND_PX`, re-encoded, so no
/// location or camera data goes with it.
async fn photo(path: &Path) -> Result<(mime_guess::mime::Mime, Vec<u8>)> {
    let path = path.to_owned();
    let made = tokio::task::spawn_blocking(move || caption::downscale(&path)).await?;
    let made = made.map_err(|e| {
        log::debug!("avatar: {e:#}");
        anyhow!(t!("That picture could not be used."))
    })?;
    Ok((made.mime, made.bytes))
}

/// People the user has a one-to-one chat with on the same network as `group` (both plain Matrix,
/// or both through the same bridge), who are not in the group; sorted by name.
pub(super) async fn candidates(client: &Client, group: &Room) -> Vec<(OwnedUserId, String)> {
    let network = super::protocol_id(group).await;
    let inside: HashSet<OwnedUserId> = group.members_no_sync(RoomMemberships::JOIN | RoomMemberships::INVITE).await.unwrap_or_default().iter().map(|m| m.user_id().to_owned()).collect();
    let mut out: Vec<(OwnedUserId, String)> = Vec::new();
    for room in client.joined_rooms() {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        let Some(person) = other_person(&room).await else { continue };
        if inside.contains(&person) || out.iter().any(|(id, _)| *id == person) || super::protocol_id(&room).await != network {
            continue;
        }
        let name = match room.get_member_no_sync(&person).await {
            Ok(Some(m)) => m.name().to_owned(),
            _ => person.localpart().to_owned(),
        };
        out.push((person, name));
    }
    out.sort_by_cached_key(|(_, name)| name.to_lowercase());
    out
}

pub(super) async fn profile(client: &Client) -> Result<Profile> {
    let account = client.account();
    let name = account.get_display_name().await?.unwrap_or_default();
    let picture = match account.get_avatar_url().await? {
        Some(url) => super::picture(client, url).await.map_err(|e| log::debug!("own picture: {e:#}")).ok(),
        None => None,
    };
    Ok(Profile { name, picture })
}

pub(super) async fn set_profile(client: &Client, mine: Mine) -> Result<()> {
    let account = client.account();
    match mine {
        Mine::Name(name) => {
            let name = clip(&name, NAME_MAX);
            account.set_display_name((!name.is_empty()).then_some(name.as_str())).await?;
        }
        Mine::Photo(path) => {
            let (mime, bytes) = photo(&path).await?;
            let url = account.upload_avatar(&mime, bytes).await?;
            account.set_avatar_url(Some(&url)).await.context("set avatar")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_features_gate_changes_and_their_absence_does_not() {
        assert!(allows(&[], "state", "m.room.name"));
        let meta = serde_json::json!({
            "state": { "m.room.name": { "level": 2 }, "m.room.avatar": { "level": 2 } },
            "member_actions": { "invite": 2, "kick": 2 }
        });
        let dm = [serde_json::json!({ "state": { "com.beeper.disappearing_timer": { "level": 2 } } })];
        let feats = [meta];
        assert!(allows(&feats, "state", "m.room.name"));
        assert!(!allows(&feats, "state", "m.room.topic"), "Messenger groups take no description");
        assert!(allows(&feats, "member_actions", "kick"));
        assert!(!allows(&dm, "member_actions", "invite"));
        assert!(!allows(&dm, "state", "m.room.name"));
        let rejected = serde_json::json!({ "member_actions": { "kick": -2 } });
        assert!(!allows(&[rejected], "member_actions", "kick"));
    }

    #[test]
    fn text_is_trimmed_and_capped() {
        assert_eq!(clip("  Ann  ", NAME_MAX), "Ann");
        assert_eq!(clip(&"ż".repeat(NAME_MAX + 5), NAME_MAX).chars().count(), NAME_MAX);
    }
}
