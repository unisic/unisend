//! Forwarding: a message's content sent again as a new event in other chats.
//!
//! As Element's ForwardDialog does (`transformEvent`), the content is copied with its relations
//! dropped; a file is not uploaded again, the new event points at the same `mxc://` URI (with the
//! same key, for an encrypted file), which is self-contained and not tied to the source room.
//! Nothing marks the copy as forwarded: no spec or bridge defines a marker for a sent message
//! (MSC2723 is an open proposal; mautrix-whatsapp only reads WhatsApp's own flag).

use anyhow::{Result, bail};
use matrix_sdk::{
    Room,
    ruma::{
        EventId,
        events::AnyMessageLikeEventContent,
        events::room::message::{RoomMessageEventContent, sanitize::remove_plain_reply_fallback},
        serde::Raw,
    },
};
use matrix_sdk_ui::timeline::Timeline;
use serde_json::{Map, Value, json};
use crate::i18n::{t, tn};

/// Most chats one forward goes to, and most messages: what the UI lets the user pick.
pub const MAX_CHATS: usize = 5;
pub const MAX_MESSAGES: usize = 100;

/// Keys that belong to the source message or chat, not to what it says: relations point into the
/// source chat, a per-message profile is another sender's name and picture, a timer is the source
/// chat's (mautrix-go `event/message.go`, `MessageEventContent`).
const DROPPED: [&str; 7] = [
    "m.relates_to",
    "m.new_content",
    "com.beeper.per_message_profile",
    "com.beeper.message_send_retry",
    "com.beeper.disappearing_timer",
    "com.beeper.action_message",
    "com.beeper.stream",
];

/// The event type and content to send for a message, from its event and, when it was edited, the
/// content of its latest edit (`m.new_content`).
pub(super) fn transform(event: &Value, edited: Option<&Value>) -> Result<(String, Map<String, Value>)> {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind != "m.room.message" && kind != "m.sticker" {
        bail!(t!("This message cannot be forwarded"));
    }
    let original = event.get("content").and_then(Value::as_object);
    let Some(mut content) = edited.or(event.get("content")).and_then(Value::as_object).cloned() else {
        bail!(t!("This message cannot be forwarded"));
    };
    if content.contains_key("com.beeper.view_limited") {
        bail!(t!("Media that can be viewed once cannot be forwarded"));
    }
    let replied = original.and_then(|c| c.get("m.relates_to")).is_some_and(|r| r.get("m.in_reply_to").is_some());
    for key in DROPPED {
        content.remove(key);
    }
    // Old clients quote the message replied to inside the reply itself (removed in Matrix 1.13).
    if replied {
        if let Some(Value::String(body)) = content.get_mut("body") {
            *body = remove_plain_reply_fallback(body).to_owned();
        }
        if let Some(Value::String(html)) = content.get_mut("formatted_body") {
            *html = remove_html_reply_fallback(html);
        }
    }
    // Whoever the message mentioned is not pinged again; an empty `m.mentions` also turns off the
    // older rules that match names in the text (MSC3952).
    content.insert("m.mentions".to_owned(), json!({}));
    Ok((kind.to_owned(), content))
}

fn remove_html_reply_fallback(html: &str) -> String {
    match (html.find("<mx-reply>"), html.find("</mx-reply>")) {
        (Some(start), Some(end)) if start < end => format!("{}{}", &html[..start], &html[end + "</mx-reply>".len()..]),
        _ => html.to_owned(),
    }
}

/// Queues each message, in the order given, in each chat. Returns how many chats got them all.
/// Nothing is sent when one of the messages cannot be forwarded.
pub(super) async fn forward(timeline: &Timeline, ids: &[String], to: &[Room]) -> Result<usize> {
    if ids.is_empty() || ids.len() > MAX_MESSAGES || to.is_empty() || to.len() > MAX_CHATS {
        bail!(t!("Choose up to {chats} chats and {messages} messages", chats = MAX_CHATS, messages = MAX_MESSAGES));
    }
    let mut events = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(item) = timeline.item_by_event_id(&EventId::parse(id)?).await else { bail!(t!("That message is no longer loaded")) };
        let Some(original) = item.original_json() else { bail!(t!("That message has not been sent yet")) };
        let event: Value = original.deserialize_as_unchecked()?;
        let edit: Option<Value> = item.latest_edit_json().map(|edit| edit.deserialize_as_unchecked::<Value>()).transpose()?;
        let mut new_content = edit.and_then(|mut edit| edit.get_mut("content")?.get_mut("m.new_content").map(Value::take));
        // The SDK keeps an edit's JSON only once the server has sent it back; until then the text
        // it shows is all there is.
        if new_content.is_none()
            && let Some(message) = item.content().as_message().filter(|m| m.is_edited())
        {
            new_content = Some(serde_json::to_value(RoomMessageEventContent::new(message.msgtype().clone()))?);
        }
        events.push(transform(&event, new_content.as_ref())?);
    }
    let mut done = 0;
    let mut failed = None;
    for room in to {
        let mut ok = true;
        for (kind, content) in &events {
            let raw: Raw<AnyMessageLikeEventContent> = Raw::new(content)?.cast_unchecked();
            // The send queue encrypts for an encrypted chat and retries while offline.
            if let Err(e) = room.send_queue().send_raw(raw, kind.clone()).await {
                (ok, failed) = (false, Some(e));
                break;
            }
        }
        done += usize::from(ok);
    }
    match failed {
        Some(e) => bail!(tn!(to.len(), "Forwarded to {done} of {n} chat: {error}", "Forwarded to {done} of {n} chats: {error}", done = done, error = e)),
        None => Ok(done),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_keeps_the_text_and_file_and_drops_where_it_came_from() {
        let event = json!({
            "type": "m.room.message",
            "content": {
                "msgtype": "m.image", "body": "> <@a:x> hi\n\ncat", "format": "org.matrix.custom.html",
                "formatted_body": "<mx-reply><blockquote>hi</blockquote></mx-reply><b>cat</b>",
                "file": {"url": "mxc://x/y", "key": {"k": "secret"}, "iv": "i", "hashes": {}, "v": "v2"},
                "m.relates_to": {"m.in_reply_to": {"event_id": "$e"}},
                "m.mentions": {"user_ids": ["@a:x"]},
                "com.beeper.per_message_profile": {"displayname": "Someone"},
                "com.beeper.linkpreviews": [],
            }
        });
        let (kind, content) = transform(&event, None).unwrap();
        assert_eq!(kind, "m.room.message");
        assert_eq!(content["body"], "cat");
        assert_eq!(content["formatted_body"], "<b>cat</b>");
        assert_eq!(content["file"]["url"], "mxc://x/y");
        assert_eq!(content["m.mentions"], json!({}));
        assert!(content.get("m.relates_to").is_none());
        assert!(content.get("com.beeper.per_message_profile").is_none());
        assert!(content.get("com.beeper.linkpreviews").is_some());
    }

    #[test]
    fn an_edited_message_is_forwarded_as_it_now_reads() {
        let event = json!({"type": "m.room.message", "content": {"msgtype": "m.text", "body": "> <@a:x> not a reply\nold"}});
        let edit = json!({"msgtype": "m.text", "body": "new"});
        assert_eq!(transform(&event, Some(&edit)).unwrap().1["body"], "new");
        // Not a reply: a quote that looks like a fallback is kept.
        assert_eq!(transform(&event, None).unwrap().1["body"], "> <@a:x> not a reply\nold");
    }

    #[test]
    fn polls_and_view_once_media_are_not_forwarded() {
        assert!(transform(&json!({"type": "org.matrix.msc3381.poll.start", "content": {}}), None).is_err());
        let once = json!({"type": "m.room.message", "content": {"msgtype": "m.image", "body": "x", "com.beeper.view_limited": {}}});
        assert!(transform(&once, None).is_err());
        assert!(transform(&json!({"type": "m.sticker", "content": {"body": "s", "url": "mxc://x/y"}}), None).is_ok());
    }
}
