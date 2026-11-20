//! Blocking a person is the Matrix ignore list (`m.ignored_user_list` account data): the server
//! stops delivering their messages and invites to this account, on every device. A bridged
//! network is not told, so the person is not blocked there.
//!
//! A one-to-one chat whose bridge offers it (`block_user` in `com.beeper.room_features`) blocks on
//! the network instead, as Beeper's apps do: `com.beeper.block_user` (`{"block": bool}`) in the
//! chat, and the bridge says the outcome with `com.beeper.user_blocked` in the channel of its
//! `m.bridge` state (mautrix-go `bridgev2/portal.go` `handleMatrixBlockUser`, `event/state.go`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use matrix_sdk::{Client, Room, ruma::{OwnedUserId, RoomId, UserId, events::{StateEventType, ignored_user_list::IgnoredUserListEventContent}}};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::Ev;

/// A longer list is shown cut here; the names are looked up one by one.
pub const MAX_BLOCKED: usize = 200;
const NAME_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
pub struct Blocked {
    pub id: OwnedUserId,
    /// The profile's display name, or the ID's local part until it is known.
    pub name: String,
}

/// The valid IDs of an ignore list, sorted, at most `MAX_BLOCKED`.
pub(super) fn parse(ids: &[String]) -> Vec<OwnedUserId> {
    let mut out: Vec<OwnedUserId> = ids.iter().filter_map(|id| UserId::parse(id).ok()).collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_BLOCKED);
    out
}

pub(super) async fn set(client: &Client, user: &UserId, on: bool) -> Result<()> {
    if on { client.account().ignore_user(user).await? } else { client.account().unignore_user(user).await? }
    Ok(())
}

/// Has the chat's bridge block (or unblock) the other person on the network.
pub(super) async fn set_on_network(client: &Client, room: &RoomId, on: bool) -> Result<()> {
    let room = client.get_room(room).ok_or_else(|| anyhow::anyhow!(crate::i18n::t!("Unknown chat")))?;
    room.send_raw("com.beeper.block_user", json!({ "block": on })).await?;
    Ok(())
}

/// The bridge says the other person of this one-to-one chat is blocked on the network.
pub(crate) async fn on_network(room: &Room) -> bool {
    let events = room.get_state_events(StateEventType::from("m.bridge")).await.unwrap_or_default();
    events.iter().any(|e| {
        let matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState::Sync(raw) = e else { return false };
        raw.get_field::<Value>("content").ok().flatten().is_some_and(|c| user_blocked(&c))
    })
}

fn user_blocked(content: &Value) -> bool {
    content.pointer("/channel/com.beeper.user_blocked").and_then(Value::as_bool).unwrap_or(false)
}

/// Tells the UI the blocked people now and whenever the list changes, on this device or another.
pub(super) async fn watch(client: Client, ev: mpsc::Sender<Ev>, last: Arc<Mutex<Vec<Blocked>>>) {
    // Subscribed before the read, so a change in between is not lost.
    let mut changes = client.subscribe_to_ignore_user_list_changes();
    let mut ids: Vec<String> = match client.account().account_data::<IgnoredUserListEventContent>().await {
        Ok(Some(raw)) => raw.deserialize().map(|c| c.ignored_users.into_keys().map(|id| id.to_string()).collect()).unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut names: HashMap<OwnedUserId, String> = HashMap::new();
    loop {
        let list = parse(&ids);
        names.retain(|id, _| list.contains(id));
        let publish = |names: &HashMap<OwnedUserId, String>| {
            let blocked: Vec<Blocked> = list.iter().map(|id| Blocked { id: id.clone(), name: names.get(id).cloned().unwrap_or_else(|| id.localpart().to_owned()) }).collect();
            *last.lock().unwrap() = blocked.clone();
            Ev::Blocked(blocked)
        };
        if ev.send(publish(&names)).await.is_err() {
            return;
        }
        let missing: Vec<&OwnedUserId> = list.iter().filter(|id| !names.contains_key(*id)).collect();
        if !missing.is_empty() {
            for id in missing {
                let found = tokio::time::timeout(NAME_WAIT, client.account().fetch_user_profile_of(id)).await;
                if let Ok(Ok(profile)) = found
                    && let Some(name) = profile.get("displayname").and_then(|v| v.as_str()).map(str::trim).filter(|n| !n.is_empty())
                {
                    names.insert(id.clone(), name.to_owned());
                }
            }
            if ev.send(publish(&names)).await.is_err() {
                return;
            }
        }
        match changes.next().await {
            Some(next) => ids = next,
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sorts_drops_invalid_and_duplicates_and_caps() {
        let ids = ["@b:x.org", "nonsense", "@a:x.org", "@b:x.org"].map(String::from);
        let got: Vec<String> = parse(&ids).iter().map(ToString::to_string).collect();
        assert_eq!(got, ["@a:x.org", "@b:x.org"]);
        let many: Vec<String> = (0..MAX_BLOCKED + 10).map(|n| format!("@u{n}:x.org")).collect();
        assert_eq!(parse(&many).len(), MAX_BLOCKED);
    }

    #[test]
    fn network_block_is_read_from_the_bridge_channel() {
        assert!(user_blocked(&json!({ "bridgebot": "@b:x", "channel": { "id": "1", "com.beeper.user_blocked": true } })));
        assert!(!user_blocked(&json!({ "channel": { "id": "1" } })));
        assert!(!user_blocked(&json!({ "com.beeper.user_blocked": true })), "only the channel section counts");
    }
}
