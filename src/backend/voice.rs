//! Voice messages: what is sent, and the waveform a received one carries.
//!
//! The event is MSC3245 v1, the shape Element sends and the mautrix bridges read: `m.audio` with
//! `org.matrix.msc3245.voice` and `org.matrix.msc1767.audio` (duration in ms, waveform 0 to 1024).
//! matrix-sdk builds both blocks from `AttachmentInfo::Voice`.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use matrix_sdk::Room;
use matrix_sdk::attachment::{AttachmentConfig, AttachmentInfo, BaseAudioInfo};
use matrix_sdk::room::reply::{EnforceThread, Reply};
use matrix_sdk::ruma::events::room::message::AddMentions;
use matrix_sdk::ruma::{EventId, UInt};

/// Waveform samples sent with a recording; MSC3246 asks for 30 to 120.
pub const SENT_SAMPLES: usize = 64;
/// Bars a received waveform is drawn with.
pub const BARS: usize = 42;

/// Uploads a recording as a voice message, then deletes it. `levels` are 0 to 1, one per tick.
pub(super) async fn send(room: &Room, path: &Path, duration: Duration, levels: Vec<f32>, reply_to: Option<String>) -> Result<()> {
    let data = tokio::fs::read(path).await?;
    // Android records Ogg Opus from 10 on; before that, AAC in MP4, which the WhatsApp bridge
    // takes only as `audio/mp4` (mime_guess says `audio/m4a`).
    let ogg = path.extension().is_some_and(|e| e == "ogg");
    let (name, mime) = if ogg { ("Voice message.ogg", "audio/ogg") } else { ("Voice message.m4a", "audio/mp4") };
    let info = BaseAudioInfo { duration: Some(duration), size: UInt::new(data.len() as u64), waveform: Some(resample(&levels, SENT_SAMPLES)) };
    let mut config = AttachmentConfig::new().info(AttachmentInfo::Voice(info));
    if let Some(id) = reply_to {
        let reply = Reply { event_id: EventId::parse(id)?, enforce_thread: EnforceThread::MaybeThreaded, add_mentions: AddMentions::Yes };
        config = config.reply(Some(reply));
    }
    room.send_attachment(name, &mime.parse()?, data, config).await?;
    // Kept when sending failed, so it is not lost; the recorder clears day-old ones.
    if let Err(e) = tokio::fs::remove_file(path).await {
        log::debug!("voice file: {e}");
    }
    Ok(())
}

/// `n` values from `levels`, each the loudest of its share; a short list is stretched.
pub fn resample(levels: &[f32], n: usize) -> Vec<f32> {
    if levels.is_empty() {
        return Vec::new();
    }
    (0..n)
        .map(|i| {
            let start = i * levels.len() / n;
            let end = ((i + 1) * levels.len() / n).max(start + 1);
            levels[start..end].iter().copied().fold(0., f32::max)
        })
        .collect()
}

/// A received waveform as `BARS` heights, 0 to 255, scaled to its own peak: senders disagree on the
/// range (Element 0 to 1024, the WhatsApp bridge WhatsApp's own 0 to 100).
pub fn bars(waveform: impl IntoIterator<Item = u64>) -> Vec<u8> {
    let waveform: Vec<u64> = waveform.into_iter().collect();
    let peak = waveform.iter().copied().max().unwrap_or(0);
    if peak == 0 {
        return Vec::new();
    }
    let levels: Vec<f32> = waveform.iter().map(|&v| v as f32 / peak as f32).collect();
    resample(&levels, BARS).into_iter().map(|v| (v * 255.).round() as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_takes_peaks_and_stretches() {
        assert_eq!(resample(&[0.1, 0.9, 0.2, 0.4], 2), vec![0.9, 0.4]);
        assert_eq!(resample(&[0.5, 1.], 4), vec![0.5, 0.5, 1., 1.]);
        assert!(resample(&[], 4).is_empty());
        assert_eq!(resample(&vec![0.3; 9000], SENT_SAMPLES).len(), SENT_SAMPLES);
    }

    #[test]
    fn bars_scale_to_their_peak() {
        let b = bars((0..64).map(|i| if i == 10 { 100 } else { 50 }));
        assert_eq!(b.len(), BARS);
        assert_eq!(b.iter().copied().max(), Some(255));
        assert!(b.contains(&128));
        assert!(bars([0, 0, 0]).is_empty());
        assert!(bars([]).is_empty());
    }

    /// The event as the bridges read it (mautrix-go `event.MSC1767Audio`, `MSC3245Voice`), and as
    /// this app reads it back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local homeserver with JWT login, see backend/tests.rs"]
    async fn sends_what_the_bridges_read() {
        use matrix_sdk::Client;
        use matrix_sdk::room::MessagesOptions;
        use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

        let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
        let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
        let tag = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let dir = std::env::temp_dir().join(format!("beeper-gpui-voice-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!("voice{tag}");
        let client = Client::builder().homeserver_url(&hs).sqlite_store(dir.join("store"), None).build().await.unwrap();
        let mut data = matrix_sdk::ruma::serde::JsonObject::new();
        data.insert("token".into(), super::super::tests::jwt(&secret, &name).into());
        client.matrix_auth().login_custom("org.matrix.login.jwt", data).unwrap().await.unwrap();
        let room = client.create_room(Default::default()).await.unwrap();

        let path = dir.join("voice-1.ogg");
        std::fs::write(&path, b"OggS not really").unwrap();
        let levels: Vec<f32> = (0..300).map(|i| (i % 50) as f32 / 49.).collect();
        send(&room, &path, Duration::from_millis(6541), levels, None).await.unwrap();
        assert!(!path.exists(), "a sent recording is deleted");

        let page = room.messages(MessagesOptions::backward()).await.unwrap();
        let sent = page.chunk.iter().find_map(|ev| {
            let json: serde_json::Value = serde_json::from_str(ev.raw().json().get()).ok()?;
            (json["type"] == "m.room.message").then_some(json)
        });
        let event = sent.expect("the voice message is in the room");
        let c = &event["content"];
        assert_eq!(c["msgtype"], "m.audio");
        assert_eq!(c["body"], "Voice message.ogg");
        assert_eq!(c["info"]["mimetype"], "audio/ogg");
        assert_eq!(c["info"]["duration"], 6541);
        assert_eq!(c["info"]["size"], 15);
        assert_eq!(c["org.matrix.msc3245.voice"], serde_json::json!({}));
        assert_eq!(c["org.matrix.msc1767.audio"]["duration"], 6541);
        let wave = c["org.matrix.msc1767.audio"]["waveform"].as_array().unwrap();
        assert_eq!(wave.len(), SENT_SAMPLES);
        assert!(wave.iter().all(|v| v.as_u64().is_some_and(|v| v <= 1024)));
        assert_eq!(wave.iter().filter_map(|v| v.as_u64()).max(), Some(1024));

        let content: RoomMessageEventContent = serde_json::from_value(c.clone()).unwrap();
        let (media, caption) = super::super::content(&content.msgtype);
        let media = media.unwrap();
        assert_eq!(media.kind, super::super::MediaKind::Voice);
        assert_eq!(media.duration, Some(Duration::from_millis(6541)));
        assert_eq!(media.waveform.len(), BARS);
        assert!(caption.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
