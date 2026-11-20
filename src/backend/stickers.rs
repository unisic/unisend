//! Stickers from image packs (MSC2545), read and sent as Beeper's apps do: the account's own pack
//! (`m.image_pack` account data, `im.ponies.user_emotes` before it), the packs of the rooms it
//! names (`m.image_pack.rooms`, `im.ponies.emote_rooms`) and those of the open chat
//! (`m.room.image_pack`, `im.ponies.room_emotes`; the first wins for one state key). An image is a
//! sticker when its own `usage`, or else its pack's, is empty or names "sticker". Only what the
//! client already holds is read: no request, no pack from any other service.
//!
//! A sticker goes out as `m.sticker` pointing at the pack's own `mxc://` image, as Element sends
//! it, so nothing is uploaded again. Beeper Desktop adds `com.beeper.msc4459.image_source_packs`
//! (the pack's room and state key); that names the pack's room to everyone in the chat, so it is
//! left out.
//!
//! Cost: one store read per pack type and listed room, on opening the picker; at most
//! `MAX_PACKS` packs of `MAX_STICKERS`, about 300 B each, held by the UI while the picker lives.
//! A sticker's picture is the server's thumbnail of `THUMB_PX`, re-encoded to that size here (at
//! most 64 KB decoded each), fetched as the grid shows it.

use std::collections::HashSet;

use anyhow::Result;
use matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::{GlobalAccountDataEventType, StateEventType};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{EventId, OwnedMxcUri, RoomId, UInt};
use matrix_sdk::{Client, Room};
use serde_json::{Map, Value, json};

use crate::i18n::t;

pub const MAX_PACKS: usize = 50;
const MAX_STICKERS: usize = 200;
/// Rooms named by the account's list whose packs are read.
const MAX_ROOMS: usize = 50;
/// Side of a sticker's picture in the grid, in pixels.
pub const THUMB_PX: u32 = 128;

const ROOM_TYPES: [&str; 2] = ["m.room.image_pack", "im.ponies.room_emotes"];

#[derive(Clone, Debug, PartialEq)]
pub struct Sticker {
    pub url: OwnedMxcUri,
    /// What the sticker shows, in words: its `body`, else its shortcode.
    pub body: String,
    /// The image's `info` as the pack gives it (`mimetype`, `w`, `h`, `size`).
    pub info: Map<String, Value>,
}

impl Sticker {
    pub fn mime(&self) -> Option<&str> {
        self.info.get("mimetype").and_then(Value::as_str)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    pub name: String,
    pub stickers: Vec<Sticker>,
}

/// The stickers of one pack's content, `name` when it names itself nothing.
fn pack(content: &Value, name: &str) -> Option<Pack> {
    let meta = content.get("pack");
    let usage = |v: Option<&Value>| v.and_then(|v| v.get("usage")).and_then(Value::as_array).map(|u| u.iter().any(|x| x == "sticker") || u.is_empty());
    let pack_takes = usage(meta).unwrap_or(true);
    let mut seen = HashSet::new();
    let mut stickers = Vec::new();
    for (code, image) in content.get("images")?.as_object()? {
        let Some(url) = image.get("url").and_then(Value::as_str).filter(|u| u.starts_with("mxc://")) else { continue };
        if !usage(Some(image)).unwrap_or(pack_takes) || !seen.insert(url) {
            continue;
        }
        let body = image.get("body").and_then(Value::as_str).filter(|b| !b.is_empty()).unwrap_or(code);
        let info = image.get("info").and_then(Value::as_object).cloned().unwrap_or_default();
        stickers.push(Sticker { url: url.into(), body: body.to_owned(), info });
        if stickers.len() == MAX_STICKERS {
            break;
        }
    }
    let name = meta.and_then(|m| m.get("display_name")).and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(name);
    (!stickers.is_empty()).then(|| Pack { name: name.to_owned(), stickers })
}

/// (room, state key) of every pack the account's list names.
fn listed(content: &Value) -> Vec<(String, String)> {
    let Some(rooms) = content.get("rooms").and_then(Value::as_object) else { return Vec::new() };
    rooms.iter().flat_map(|(room, keys)| keys.as_object().into_iter().flat_map(|k| k.keys()).map(move |key| (room.clone(), key.clone()))).collect()
}

async fn account(client: &Client, kinds: [&str; 2]) -> Option<Value> {
    for kind in kinds {
        if let Ok(Some(raw)) = client.account().account_data_raw(GlobalAccountDataEventType::from(kind)).await
            && let Ok(v) = raw.deserialize_as::<Value>()
        {
            return Some(v);
        }
    }
    None
}

/// The room's packs as (state key, content), one per state key, `m.room.image_pack` first.
async fn room_packs(room: &Room) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = Vec::new();
    for kind in ROOM_TYPES {
        let Ok(events) = room.get_state_events(StateEventType::from(kind)).await else { continue };
        for e in events {
            let RawAnySyncOrStrippedState::Sync(raw) = e else { continue };
            let (Ok(Some(key)), Ok(Some(content))) = (raw.get_field::<String>("state_key"), raw.get_field::<Value>("content")) else { continue };
            if !out.iter().any(|(k, _)| *k == key) {
                out.push((key, content));
            }
        }
    }
    out
}

/// The sticker packs offered in `open`: the account's, the listed rooms', then the chat's own.
pub(super) async fn packs(client: &Client, open: &Room) -> Vec<Pack> {
    let mut out = Vec::new();
    if let Some(own) = account(client, ["m.image_pack", "im.ponies.user_emotes"]).await {
        out.extend(pack(&own, t!("My stickers")));
    }
    let mut done: HashSet<(String, String)> = HashSet::new();
    let list = account(client, ["m.image_pack.rooms", "im.ponies.emote_rooms"]).await.map(|l| listed(&l)).unwrap_or_default();
    let mut wanted: Vec<&str> = list.iter().map(|(room, _)| room.as_str()).collect();
    wanted.dedup();
    for room_id in wanted.into_iter().take(MAX_ROOMS) {
        let Some(room) = RoomId::parse(room_id).ok().and_then(|id| client.get_room(&id)) else { continue };
        let name = room.cached_display_name().map(|n| n.to_string()).unwrap_or_default();
        for (key, content) in room_packs(&room).await {
            if list.iter().any(|(r, k)| r == room_id && *k == key) && done.insert((room_id.to_owned(), key.clone())) {
                out.extend(pack(&content, if key.is_empty() { &name } else { &key }));
            }
        }
    }
    let name = open.cached_display_name().map(|n| n.to_string()).unwrap_or_default();
    for (key, content) in room_packs(open).await {
        if done.insert((open.room_id().to_string(), key.clone())) {
            out.extend(pack(&content, if key.is_empty() { &name } else { &key }));
        }
    }
    out.truncate(MAX_PACKS);
    out
}

/// A sticker's picture for the grid: the server's thumbnail, at most `THUMB_PX` a side.
pub(super) async fn thumb(client: &Client, url: OwnedMxcUri) -> Result<Vec<u8>> {
    let side = UInt::from(THUMB_PX);
    let request = MediaRequestParameters { source: MediaSource::Plain(url), format: MediaFormat::Thumbnail(MediaThumbnailSettings::new(side, side)) };
    let bytes = client.media().get_media_content(&request, true).await?;
    // Some servers answer with the original upload, and animated ones stay animated: one still frame.
    tokio::task::spawn_blocking(move || super::shrink(&bytes, THUMB_PX)).await?
}

/// The `m.sticker` content for `sticker`, replying to `reply_to`.
fn content(sticker: &Sticker, reply_to: Option<&EventId>) -> Value {
    let mut content = json!({ "body": sticker.body, "url": sticker.url, "info": sticker.info });
    if let Some(id) = reply_to {
        content["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": id } });
    }
    content
}

/// Queues `sticker` in `room`: the chat shows it at once, as any message being sent.
pub(super) async fn send(room: &Room, sticker: &Sticker, reply_to: Option<String>) -> Result<()> {
    let reply_to = reply_to.map(EventId::parse).transpose()?;
    let raw: Raw<matrix_sdk::ruma::events::AnyMessageLikeEventContent> = Raw::new(&content(sticker, reply_to.as_deref()))?.cast_unchecked();
    room.send_queue().send_raw(raw, "m.sticker".to_owned()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::event_id;
    use serde_json::json;

    use super::*;

    #[test]
    fn a_pack_offers_its_stickers_once_each() {
        let content = json!({
            "pack": { "display_name": "Cats", "usage": ["emoticon", "sticker"] },
            "images": {
                "cat": { "url": "mxc://x/cat", "body": "A cat", "info": { "mimetype": "image/webp", "w": 256 } },
                "cat2": { "url": "mxc://x/cat" },
                "smile": { "url": "mxc://x/smile", "usage": ["emoticon"] },
                "bare": { "url": "mxc://x/bare" },
                "web": { "url": "https://example.com/a.png" }
            }
        });
        let p = pack(&content, "fallback").unwrap();
        assert_eq!(p.name, "Cats");
        let got: Vec<(&str, &str)> = p.stickers.iter().map(|s| (s.url.as_str(), s.body.as_str())).collect();
        assert_eq!(got, [("mxc://x/cat", "A cat"), ("mxc://x/bare", "bare")], "in the pack's own order");
        assert_eq!(p.stickers[0].mime(), Some("image/webp"));
        let emotes = json!({ "pack": { "usage": ["emoticon"] }, "images": { "a": { "url": "mxc://x/a" } } });
        assert!(pack(&emotes, "x").is_none(), "an emoticon pack has no stickers");
        let named = json!({ "images": { "a": { "url": "mxc://x/a" } } });
        assert_eq!(pack(&named, "Room").unwrap().name, "Room");
    }

    #[test]
    fn the_account_list_names_room_and_state_key() {
        let list = json!({ "rooms": { "!a:x": { "": {}, "cats": {} }, "!b:x": {} } });
        let mut got = listed(&list);
        got.sort();
        assert_eq!(got, [("!a:x".to_owned(), String::new()), ("!a:x".to_owned(), "cats".to_owned())]);
    }

    #[test]
    fn a_sticker_points_at_the_packs_image() {
        let s = Sticker { url: "mxc://x/cat".into(), body: "A cat".into(), info: json!({ "w": 256 }).as_object().unwrap().clone() };
        assert_eq!(content(&s, None), json!({ "body": "A cat", "url": "mxc://x/cat", "info": { "w": 256 } }));
        assert_eq!(content(&s, Some(event_id!("$q")))["m.relates_to"], json!({ "m.in_reply_to": { "event_id": "$q" } }));
    }
}
