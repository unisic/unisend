//! A message's earlier versions, for the bubble's "Edited" mark, as Signal lists them: the
//! original and each edit (`m.replace`, Matrix spec "Event replacements") with its time. The SDK's
//! timeline keeps only the newest version, so the edits are asked of the server once per opening
//! (`/relations/{event}/m.replace`) and decrypted here. An edit counts only from the original's
//! sender, as the spec requires; its text is the edit's `m.new_content`. Spoilers stay covered.

use anyhow::{Context as _, Result};
use matrix_sdk::Room;
use matrix_sdk::room::{IncludeRelations, RelationsOptions};
use matrix_sdk::ruma::api::Direction;
use matrix_sdk::ruma::events::relation::RelationType;
use matrix_sdk::ruma::events::room::message::{MessageType, Relation};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::events::room::message::OriginalSyncRoomMessageEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{EventId, UInt};

use super::{content, html, rich};

/// Edits read, the newest: one request, no paging.
const MAX_EDITS: u32 = 50;

/// One version of a message: when it was sent and what it said.
#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub ts: u64,
    pub text: String,
}

/// What a version reads as: the formatted body's text with its spoilers masked, else the body.
fn text(msgtype: &MessageType) -> String {
    match html(msgtype).and_then(rich::parse) {
        Some((plain, rich)) => rich::masked(&plain, Some(&rich)),
        None => content(msgtype).1,
    }
}

fn message(raw: &Raw<AnySyncTimelineEvent>) -> Option<OriginalSyncRoomMessageEvent> {
    match raw.deserialize().ok()? {
        AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(e))) => Some(e),
        _ => None,
    }
}

/// The original of `id`, then its edits, oldest first.
pub(super) async fn history(room: &Room, id: &EventId) -> Result<Vec<Version>> {
    let original = room.load_or_fetch_event(id, None).await?;
    let opts = RelationsOptions {
        include_relations: IncludeRelations::RelationsOfType(RelationType::Replacement),
        dir: Direction::Backward,
        limit: Some(UInt::from(MAX_EDITS)),
        ..RelationsOptions::default()
    };
    let chunk = room.relations(id.to_owned(), opts).await?.chunk;
    versions(id, original.raw(), chunk.iter().map(|ev| ev.raw())).context("not a message")
}

fn versions<'a>(id: &EventId, original: &Raw<AnySyncTimelineEvent>, edits: impl Iterator<Item = &'a Raw<AnySyncTimelineEvent>>) -> Option<Vec<Version>> {
    let first = message(original)?;
    let mut out: Vec<Version> = edits
        .filter_map(message)
        .filter(|e| e.sender == first.sender)
        .filter_map(|e| match e.content.relates_to {
            Some(Relation::Replacement(r)) if r.event_id == id => Some(Version { ts: u64::from(e.origin_server_ts.0), text: text(&r.new_content.msgtype) }),
            _ => None,
        })
        .collect();
    out.sort_by_key(|v| v.ts);
    out.insert(0, Version { ts: u64::from(first.origin_server_ts.0), text: text(&first.content.msgtype) });
    Some(out)
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::event_id;
    use serde_json::json;

    use super::*;

    fn raw(v: serde_json::Value) -> Raw<AnySyncTimelineEvent> {
        Raw::from_json(serde_json::value::to_raw_value(&v).unwrap())
    }

    fn edit(id: &str, sender: &str, ts: u64, body: &str, html: Option<&str>) -> Raw<AnySyncTimelineEvent> {
        let mut new = json!({ "msgtype": "m.text", "body": body });
        if let Some(html) = html {
            new["format"] = json!("org.matrix.custom.html");
            new["formatted_body"] = json!(html);
        }
        raw(json!({
            "type": "m.room.message", "event_id": id, "sender": sender, "origin_server_ts": ts,
            "content": { "msgtype": "m.text", "body": format!("* {body}"), "m.new_content": new,
                "m.relates_to": { "rel_type": "m.replace", "event_id": "$orig" } },
        }))
    }

    #[test]
    fn the_senders_edits_follow_the_original_oldest_first() {
        let original = raw(json!({
            "type": "m.room.message", "event_id": "$orig", "sender": "@ann:x", "origin_server_ts": 1,
            "content": { "msgtype": "m.text", "body": "helo" },
        }));
        let edits = [
            edit("$e3", "@ann:x", 30, "hello ||all||", Some("hello <span data-mx-spoiler>all</span>")),
            edit("$e2", "@bob:x", 20, "hijacked", None),
            edit("$e1", "@ann:x", 10, "hello", None),
        ];
        let got = versions(event_id!("$orig"), &original, edits.iter()).unwrap();
        let got: Vec<(u64, &str)> = got.iter().map(|v| (v.ts, v.text.as_str())).collect();
        assert_eq!(got, [(1, "helo"), (10, "hello"), (30, "hello \u{2588}\u{2588}\u{2588}")]);
        assert!(versions(event_id!("$other"), &original, edits.iter()).unwrap().len() == 1, "edits of another event");
    }
}
