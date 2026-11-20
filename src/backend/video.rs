//! What a video's event says about how to play it, and a bridged GIF made into a looping GIF
//! for its bubble.
//!
//! Cost of a loop: the clip (at most `MAX_LOOP_SOURCE`) is fetched once, written to `media/` for
//! the decoder and deleted after it; decoding and encoding take up to `MAX_LOOP_FRAMES` frames of
//! at most `LOOP_PX` on one blocking thread, about a second; the GIF stays within
//! `linkprev::MAX_MOVING` decoded, and the last `KEPT_LOOPS` are kept in memory.

use std::collections::VecDeque;
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use image::codecs::gif::{GifEncoder, Repeat};

use super::linkprev::MAX_MOVING;

mod frames;

/// The longer side of a loop's frames: about a bubble's width.
const LOOP_PX: u32 = 320;
/// Ten frames a second.
const STEP_MS: u32 = 100;
/// A longer clip loops its first five seconds.
const MAX_LOOP_FRAMES: u32 = 50;
/// A larger clip keeps its poster: it is downloaded whole to be decoded.
pub(super) const MAX_LOOP_SOURCE: u64 = 8 * 1024 * 1024;
/// Loops made, so one scrolled away and back is not decoded again.
const KEPT_LOOPS: usize = 8;

static LOOPS: Mutex<VecDeque<(String, Vec<u8>)>> = Mutex::new(VecDeque::new());

/// The loop made earlier for event `id`.
pub(super) fn kept(id: &str) -> Option<Vec<u8>> {
    let loops = LOOPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    loops.iter().find(|(key, _)| key == id).map(|(_, gif)| gif.clone())
}

pub(super) fn keep(id: &str, gif: Vec<u8>) {
    let mut loops = LOOPS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    loops.retain(|(key, _)| key != id);
    if loops.len() >= KEPT_LOOPS {
        loops.pop_front();
    }
    loops.push_back((id.to_owned(), gif));
}

/// Which frames a clip of `dims` lasting `duration` loops with; none without a size.
fn plan((width, height): (u32, u32), duration: Option<Duration>) -> Option<frames::Plan> {
    let long = width.max(height);
    if long == 0 {
        return None;
    }
    let scale = |side: u32| (u64::from(side) * u64::from(LOOP_PX.min(long)) / u64::from(long)).max(1) as u32;
    let (width, height) = (scale(width), scale(height));
    let fit = u32::try_from(MAX_MOVING / (u64::from(width) * u64::from(height) * 4)).unwrap_or(u32::MAX);
    let lasting = duration.map_or(MAX_LOOP_FRAMES, |d| u32::try_from(d.as_millis().div_ceil(u128::from(STEP_MS))).unwrap_or(u32::MAX));
    let count = lasting.min(MAX_LOOP_FRAMES).min(fit);
    (count > 1).then_some(frames::Plan { width, height, step_ms: STEP_MS, count })
}

/// The clip at `path` as a GIF that loops; none where fewer than two frames could be read.
pub(super) fn looping(path: &Path, dims: (u32, u32), duration: Option<Duration>) -> Option<Vec<u8>> {
    let plan = plan(dims, duration)?;
    let frames = frames::frames(path, plan);
    if frames.len() < 2 {
        return None;
    }
    let delay = image::Delay::from_numer_denom_ms(plan.step_ms, 1);
    let mut out = Vec::new();
    let encoded = (|| {
        let mut encoder = GifEncoder::new_with_speed(Cursor::new(&mut out), 30);
        encoder.set_repeat(Repeat::Infinite)?;
        encoder.encode_frames(frames.into_iter().map(|frame| image::Frame::from_parts(frame, 0, 0, delay)))
    })();
    match encoded {
        Ok(()) => Some(out),
        Err(e) => {
            log::debug!("loop: {e}");
            None
        }
    }
}

/// Whether the sender's network shows this video as a GIF: on repeat and without sound. The
/// mautrix bridges mark those in `info` with `fi.mau.gif` (WhatsApp, Signal, Meta), and WhatsApp
/// and Meta add `fi.mau.loop`. `content` is the event's content.
pub(super) fn looped(content: &serde_json::Value) -> bool {
    let flag = |key| content.pointer(&format!("/info/{key}")).and_then(serde_json::Value::as_bool) == Some(true);
    flag("fi.mau.gif") || flag("fi.mau.loop")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::time::Duration;

    use super::{LOOP_PX, MAX_LOOP_FRAMES, MAX_MOVING, looped, plan};

    #[test]
    fn loops_are_small_short_and_within_the_budget() {
        let wide = plan((1280, 720), Some(Duration::from_millis(1250))).unwrap();
        assert_eq!((wide.width, wide.height, wide.count), (LOOP_PX, 180, 13));
        let tiny = plan((100, 50), None).unwrap();
        assert_eq!((tiny.width, tiny.height, tiny.count), (100, 50, MAX_LOOP_FRAMES), "never scaled up, unknown length takes the most");
        let long = plan((LOOP_PX, LOOP_PX), Some(Duration::from_secs(600))).unwrap();
        assert!(u64::from(long.width * long.height * 4 * long.count) <= MAX_MOVING);
        assert!(plan((0, 0), None).is_none());
        assert!(plan((640, 480), Some(Duration::from_millis(50))).is_none(), "one frame is a still");
    }

    /// Needs a video file and the system's decoders: `LOOP_CLIP=<640x360, 2 s> cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn a_clip_becomes_a_moving_gif() {
        let path = std::env::var("LOOP_CLIP").expect("LOOP_CLIP");
        let gif = super::looping(std::path::Path::new(&path), (640, 360), Some(Duration::from_secs(2))).expect("a loop");
        let (width, height, frames) = crate::backend::linkprev::gif_frames(&gif).expect("a GIF");
        assert_eq!((width, height), (LOOP_PX, 180));
        assert!(frames >= 15, "{frames} frames");
    }

    #[test]
    fn bridged_gifs_loop_and_videos_do_not() {
        assert!(looped(&json!({ "msgtype": "m.video", "info": { "fi.mau.gif": true } })));
        assert!(looped(&json!({ "msgtype": "m.video", "info": { "fi.mau.loop": true, "fi.mau.gif": false } })));
        assert!(!looped(&json!({ "msgtype": "m.video", "info": { "duration": 3000 } })));
        assert!(!looped(&json!({ "msgtype": "m.video", "fi.mau.gif": true })), "only inside info");
        assert!(!looped(&json!({ "msgtype": "m.video", "info": { "fi.mau.gif": "true" } })));
        assert!(!looped(&json!({ "msgtype": "m.video" })));
    }
}
