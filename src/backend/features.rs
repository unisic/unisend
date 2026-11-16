//! What the open chat's network takes from here, from the bridge's `com.beeper.room_features`
//! state event (mautrix bridgev2, any state key; schema in mautrix-go `event/capabilities.go`).
//! Levels are -2 rejected, -1 dropped, 0 unsupported (may have a fallback), 1 partial, 2 full; a
//! field the bridge leaves out is Go's zero. A room without the event is plain Matrix: it takes all.

use std::collections::HashMap;

use matrix_sdk::{Room, ruma::events::MessageLikeEventType};
use serde::Deserialize;
use serde_json::Value;

pub const KIND: &str = "com.beeper.room_features";
/// A longer list is someone else's data: Telegram's whole set is under a hundred.
const MAX_REACTIONS: usize = 256;

/// The open chat's features, as the composer and the message menus need them.
#[derive(Clone, Debug, PartialEq)]
pub struct Features {
    pub location: i64,
    pub poll: i64,
    pub reply: i64,
    pub edit: i64,
    /// Seconds after sending past which the network takes no edit or deletion.
    pub edit_max_age: Option<u64>,
    pub delete: i64,
    pub delete_max_age: Option<u64>,
    pub reaction: i64,
    /// The only reactions the chat takes; `None` takes any.
    pub allowed_reactions: Option<Vec<String>>,
    /// The bridge takes the message this app sends for each: a file, a `.vcf`, a voice note (Ogg
    /// Opus, Android's recorder), a GIF as an MP4 marked `fi.mau.gif` and one as an `image/gif`.
    pub file: bool,
    pub contact: bool,
    pub voice: bool,
    pub gif_video: bool,
    pub gif_image: bool,
    /// The caption level of `m.image`, `m.video`, `m.audio` and `m.file`, in that order.
    pub captions: [i64; 4],
    /// The user's power level lets them send messages here; below it the composer gives way.
    pub post: bool,
    /// The most characters the network takes in one message; 0 when it names no limit.
    pub max_text: usize,
    /// The most options of a poll; 0 when it names no limit.
    pub poll_max: usize,
}

impl Features {
    pub const ALL: Features = Features {
        location: 2,
        poll: 2,
        reply: 2,
        edit: 2,
        edit_max_age: None,
        delete: 2,
        delete_max_age: None,
        reaction: 2,
        allowed_reactions: None,
        file: true,
        contact: true,
        voice: true,
        gif_video: true,
        gif_image: true,
        captions: [2; 4],
        post: true,
        max_text: 0,
        poll_max: 0,
    };

    /// `as_file`: the GIF goes as an `image/gif` (Meta's networks, see `gifs::wants_gif_file`).
    pub fn gif(&self, as_file: bool) -> bool {
        if as_file { self.gif_image } else { self.gif_video }
    }

    /// The network still takes an edit of a message sent at `ts` (ms) when it is `now` (ms).
    pub fn editable(&self, ts: u64, now: u64) -> bool {
        self.edit >= 1 && young(self.edit_max_age, ts, now)
    }

    pub fn deletable(&self, ts: u64, now: u64) -> bool {
        self.delete >= 1 && young(self.delete_max_age, ts, now)
    }

    /// The network keeps the caption of a file of type `mime`, sent as the message type the SDK
    /// picks for it (`make_media_type`); below partial it is dropped or the file refused.
    pub fn keeps_caption(&self, mime: &str) -> bool {
        let ix = match mime.split('/').next().unwrap_or("") {
            "image" => 0,
            "video" => 1,
            "audio" => 2,
            _ => 3,
        };
        self.captions[ix] >= 1
    }

    /// The chat takes reaction `key` (with or without U+FE0F: Telegram's list carries it).
    pub fn reacts_with(&self, key: &str) -> bool {
        let bare = |s: &str| s.chars().filter(|&c| c != '\u{FE0F}').collect::<String>();
        self.reaction >= 1 && self.allowed_reactions.as_ref().is_none_or(|list| list.iter().any(|k| bare(k) == bare(key)))
    }
}

fn young(max_age: Option<u64>, ts: u64, now: u64) -> bool {
    max_age.is_none_or(|secs| now.saturating_sub(ts) < secs.saturating_mul(1000))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Wire {
    file: HashMap<String, WireFile>,
    location_message: i64,
    poll: i64,
    poll_max_options: i64,
    max_text_length: i64,
    reply: i64,
    edit: i64,
    edit_max_age: Option<f64>,
    delete: i64,
    delete_max_age: Option<f64>,
    reaction: i64,
    allowed_reactions: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireFile {
    mime_types: HashMap<String, i64>,
    caption: i64,
}

impl WireFile {
    /// mautrix-go `FileFeatures.GetMimeSupport`: the type, without parameters, `type/*`, `*/*`.
    fn level(&self, mime: &str) -> i64 {
        let plain = mime.split(';').next().unwrap_or(mime).trim();
        let general = mime.split_once('/').map(|(kind, _)| format!("{kind}/*"));
        [Some(mime), Some(plain), general.as_deref(), Some("*/*")].into_iter().flatten().find_map(|m| self.mime_types.get(m).copied()).unwrap_or(-2)
    }
}

/// bridgev2 (`checkMessageContentCaps`) refuses a file whose type has no entry or whose MIME type
/// is rejected; anything above that goes through.
fn takes(file: &HashMap<String, WireFile>, msgtype: &str, mime: Option<&str>) -> bool {
    file.get(msgtype).is_some_and(|f| match mime {
        Some(mime) => f.level(mime) > -2,
        None => f.mime_types.values().any(|&l| l > -2),
    })
}

/// The features in an event's content. One that cannot be read says nothing, so it takes all.
pub fn parse(content: &Value) -> Features {
    let wire: Wire = match Wire::deserialize(content) {
        Ok(wire) => wire,
        Err(e) => {
            log::debug!("room features: {e}");
            return Features::ALL;
        }
    };
    let age = |secs: Option<f64>| secs.filter(|s| s.is_finite() && *s > 0.).map(|s| s as u64);
    let mut allowed = wire.allowed_reactions;
    allowed.truncate(MAX_REACTIONS);
    let f = &wire.file;
    Features {
        location: wire.location_message,
        poll: wire.poll,
        reply: wire.reply,
        edit: wire.edit,
        edit_max_age: age(wire.edit_max_age),
        delete: wire.delete,
        delete_max_age: age(wire.delete_max_age),
        reaction: wire.reaction,
        allowed_reactions: (!allowed.is_empty()).then_some(allowed),
        file: takes(f, "m.file", None),
        contact: takes(f, "m.file", Some("text/vcard")),
        voice: takes(f, "org.matrix.msc3245.voice", Some("audio/ogg")),
        gif_video: takes(f, "fi.mau.gif", Some("video/mp4")),
        gif_image: takes(f, "m.image", Some("image/gif")),
        captions: ["m.image", "m.video", "m.audio", "m.file"].map(|t| f.get(t).map_or(0, |f| f.caption)),
        post: true,
        max_text: usize::try_from(wire.max_text_length).unwrap_or(0),
        poll_max: usize::try_from(wire.poll_max_options).unwrap_or(0),
    }
}

/// The chat's features from the state the client has, the first bridge's when there are several;
/// `None` in a room no bridge serves.
/// A room whose power levels keep the user from posting (or reacting) is answered even without a
/// bridge, with what that takes away.
pub(super) async fn read(room: &Room) -> Option<Features> {
    let features = super::disappear::state(room, KIND).await.first().map(parse);
    let (post, react) = match room.power_levels().await {
        Ok(levels) => {
            let own = room.own_user_id();
            (levels.user_can_send_message(own, MessageLikeEventType::RoomMessage), levels.user_can_send_message(own, MessageLikeEventType::Reaction))
        }
        Err(e) => {
            log::debug!("power levels: {e}");
            (true, true)
        }
    };
    restrict(features, post, react)
}

fn restrict(features: Option<Features>, post: bool, react: bool) -> Option<Features> {
    if post && react {
        return features;
    }
    let mut f = features.unwrap_or(Features::ALL);
    f.post = post;
    if !react {
        f.reaction = 0;
    }
    if !post {
        (f.reply, f.edit) = (0, 0);
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// mautrix-meta `metaCaps` (a Messenger or Instagram DM), as bridgev2 serialises it.
    fn meta_dm() -> Value {
        json!({
            "id": "fi.mau.meta.capabilities.2026_11_02+ffmpeg",
            "formatting": { "bold": 2, "italic": 2 },
            "file": {
                "m.image": { "mime_types": { "image/jpeg": 2, "image/png": 2, "image/gif": 2, "image/webp": 2 }, "caption": 2, "max_size": 8000000 },
                "m.video": { "mime_types": { "video/mp4": 2, "video/quicktime": 2 }, "caption": 2 },
                "m.audio": { "mime_types": { "audio/mpeg": 2, "audio/mp4": 2, "audio/wav": 2 }, "caption": -1 },
                "m.file": { "mime_types": { "*/*": 2 }, "caption": 2 },
                "fi.mau.gif": { "mime_types": { "image/gif": 2 }, "caption": -1 },
                "m.sticker": { "mime_types": { "image/png": 2, "image/webp": 2 }, "caption": -1 },
                "org.matrix.msc3245.voice": { "mime_types": { "audio/mp4": 2, "audio/ogg": 1 }, "caption": -1 }
            },
            "max_text_length": 20000,
            "reply": 2,
            "edit": 2,
            "edit_max_count": 5,
            "edit_max_age": 900,
            "delete": 2,
            "reaction": 2,
            "reaction_count": 1,
            "typing_notifications": true,
            "delete_chat": true,
            "block_user": true
        })
    }

    #[test]
    fn meta_dm_has_no_location_or_poll_and_sends_gifs_as_images() {
        let f = parse(&meta_dm());
        assert_eq!((f.location, f.poll, f.reply, f.edit, f.delete, f.reaction), (0, 0, 2, 2, 2, 2));
        assert_eq!((f.edit_max_age, f.delete_max_age, f.allowed_reactions.as_ref()), (Some(900), None, None));
        assert!(f.file && f.contact && f.voice && f.gif_image);
        assert!(!f.gif_video, "Meta's fi.mau.gif takes only image/gif");
        assert!(f.gif(true) && !f.gif(false));
        assert!(f.editable(0, 899_999) && !f.editable(0, 900_000));
        assert!(f.deletable(0, u64::MAX));
    }

    #[test]
    fn meta_group_without_ffmpeg_has_polls_but_no_ogg_voice() {
        let mut c = meta_dm();
        c["poll"] = json!(1);
        c["poll_multiple_choice_only"] = json!(true);
        c["file"]["org.matrix.msc3245.voice"]["mime_types"]["audio/ogg"] = json!(-2);
        let f = parse(&c);
        assert_eq!(f.poll, 1);
        assert!(!f.voice);
    }

    /// mautrix-whatsapp `whatsappDMCaps`: voice is "audio/ogg; codecs=opus" full and bare
    /// "audio/ogg" unsupported (0), which the bridge still takes.
    #[test]
    fn whatsapp_dm() {
        let f = parse(&json!({
            "file": {
                "m.image": { "mime_types": { "image/jpeg": 2, "image/png": 1, "image/webp": 1, "image/gif": 1 } },
                "org.matrix.msc3245.voice": { "mime_types": { "audio/ogg; codecs=opus": 2, "audio/ogg": 0 } },
                "fi.mau.gif": { "mime_types": { "video/mp4": 2, "image/gif": 1 } },
                "m.file": { "mime_types": { "*/*": 2 } }
            },
            "location_message": 2, "poll": 2, "poll_max_options": 12, "poll_option_max_length": 100,
            "reply": 2, "edit": 2, "edit_max_age": 900, "delete": 2, "delete_max_age": 172800,
            "reaction": 2, "reaction_count": 1,
            "disappearing_timer": { "types": ["after_send"], "timers": [86400000] }
        }));
        assert_eq!((f.location, f.poll, f.reply), (2, 2, 2));
        assert!(f.voice && f.gif(false) && f.file && f.contact);
        assert_eq!(f.delete_max_age, Some(172_800));
        assert!(f.deletable(1_000, 1_000 + 172_799_999) && !f.deletable(1_000, 1_000 + 172_800_000));
    }

    #[test]
    fn whatsapp_announcement_group_takes_no_reply() {
        let f = parse(&json!({ "reply": 0, "thread": 2, "edit": 2, "delete": 2, "reaction": 2 }));
        assert_eq!(f.reply, 0);
        assert!(!f.file && !f.contact && !f.voice && !f.gif(false) && !f.gif(true), "no file map, no files");
    }

    /// mautrix-signal `signalCapsDM`: location partial (a map link), voice Ogg only with ffmpeg.
    #[test]
    fn signal_dm() {
        let f = parse(&json!({
            "file": {
                "org.matrix.msc3245.voice": { "mime_types": { "audio/aac": 2, "audio/ogg": 1 }, "max_duration": 3600 },
                "fi.mau.gif": { "mime_types": { "image/gif": 2, "video/mp4": 2 } },
                "m.file": { "mime_types": { "*/*": 2 } }
            },
            "location_message": 1, "poll": 2, "reply": 2, "edit": 2, "edit_max_count": 10, "edit_max_age": 86400,
            "delete": 2, "delete_max_age": 86400, "reaction": 2, "reaction_count": 1
        }));
        assert_eq!((f.location, f.poll), (1, 2));
        assert!(f.voice && f.gif(false) && f.contact);
        assert_eq!((f.edit_max_age, f.delete_max_age), (Some(86_400), Some(86_400)));
    }

    /// mautrix-telegram with a chat's own list (with U+FE0F added), or reactions off.
    #[test]
    fn telegram_reactions() {
        let f = parse(&json!({ "reaction": 2, "allowed_reactions": ["\u{2764}\u{FE0F}", "👍"], "location_message": 2, "poll": 2 }));
        assert!(f.reacts_with("\u{2764}") && f.reacts_with("\u{2764}\u{FE0F}") && f.reacts_with("👍"));
        assert!(!f.reacts_with("😂"));
        let off = parse(&json!({ "reaction": -2 }));
        assert!(!off.reacts_with("👍"));
    }

    /// mautrix-meta `igCaps`: Instagram drops every caption.
    #[test]
    fn captions_kept_or_dropped() {
        let mut ig = meta_dm();
        for t in ["m.image", "m.video", "m.file"] {
            ig["file"][t]["caption"] = json!(-1);
        }
        let ig = parse(&ig);
        assert!(!ig.keeps_caption("image/jpeg") && !ig.keeps_caption("video/mp4") && !ig.keeps_caption("application/pdf"));
        let messenger = parse(&meta_dm());
        assert!(messenger.keeps_caption("image/png") && messenger.keeps_caption("video/mp4") && messenger.keeps_caption("text/vcard"));
        assert!(!messenger.keeps_caption("audio/mpeg"));
        assert!(!parse(&json!({ "reply": 2 })).keeps_caption("image/jpeg"), "a bridge that lists no caption level gives none");
        assert!(Features::ALL.keeps_caption("image/jpeg"), "a plain Matrix room keeps captions");
    }

    #[test]
    fn mime_lookup_follows_mautrix() {
        let file = WireFile { mime_types: HashMap::from([("audio/*".to_owned(), 1), ("audio/ogg".to_owned(), -2)]), caption: 0 };
        assert_eq!(file.level("audio/ogg"), -2);
        assert_eq!(file.level("audio/ogg; codecs=opus"), -2);
        assert_eq!(file.level("audio/mp4"), 1);
        assert_eq!(file.level("video/mp4"), -2);
    }

    #[test]
    fn limits_are_read_and_nonsense_is_none() {
        let f = parse(&json!({ "max_text_length": 1000, "poll_max_options": 10 }));
        assert_eq!((f.max_text, f.poll_max), (1000, 10));
        let f = parse(&json!({ "max_text_length": -5 }));
        assert_eq!((f.max_text, f.poll_max), (0, 0));
    }

    #[test]
    fn power_levels_take_away_posting_and_reacting() {
        assert_eq!(restrict(None, true, true), None);
        let read_only = restrict(None, false, false).unwrap();
        assert!((!read_only.post, read_only.reaction, read_only.reply, read_only.edit) == (true, 0, 0, 0));
        let announce = restrict(Some(parse(&json!({ "reply": 2, "reaction": 2 }))), true, false).unwrap();
        assert!(announce.post && announce.reaction == 0 && announce.reply == 2);
    }

    #[test]
    fn unreadable_or_absent_takes_all() {
        assert_eq!(parse(&json!({ "poll": "yes" })), Features::ALL);
        let ok = parse(&json!({}));
        assert_eq!((ok.poll, ok.location, ok.reply), (0, 0, 0));
        assert!(Features::ALL.reacts_with("😂") && Features::ALL.editable(0, u64::MAX));
    }
}
