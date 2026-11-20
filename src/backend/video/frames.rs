//! Frames of a short video, decoded small, for a bridged GIF that loops in its bubble
//! (`video.rs` makes them a GIF). Android reads them with `MediaMetadataRetriever`
//! (`VideoFrame.frames`); Linux with GStreamer (`decodebin`, so any codec its plugins have). macOS
//! and Windows read none: the poster stays.
//!
//! Memory: `count` frames of `width` x `height` RGBA, which the caller keeps within
//! `linkprev::MAX_MOVING`; on Android the Java array and its copy here exist together for a
//! moment (a clip shorter than planned adds one more copy in Java).
//! One video at a time per caller, on a blocking thread.

use std::path::Path;

use image::RgbaImage;

/// Which frames to take: `count` at most, `step_ms` apart from the start, scaled to the size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Plan {
    pub width: u32,
    pub height: u32,
    pub step_ms: u32,
    pub count: u32,
}

impl Plan {
    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// Cuts `bytes` into frames of `plan`'s size; a partial frame at the end is dropped.
fn split(bytes: Vec<u8>, plan: Plan) -> Vec<RgbaImage> {
    let size = plan.frame_bytes();
    if size == 0 {
        return Vec::new();
    }
    bytes.chunks_exact(size).take(plan.count as usize).filter_map(|chunk| RgbaImage::from_raw(plan.width, plan.height, chunk.to_vec())).collect()
}

/// The frames of the video at `path`; none where the platform cannot decode it.
pub(super) fn frames(path: &Path, plan: Plan) -> Vec<RgbaImage> {
    let bytes = grab(path, plan).unwrap_or_else(|e| {
        log::debug!("video frames: {e}");
        Vec::new()
    });
    split(bytes, plan)
}

#[cfg(target_os = "android")]
fn grab(path: &Path, plan: Plan) -> Result<Vec<u8>, String> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JByteArray, JValue};

    let int = |n: u32| i32::try_from(n).map_err(|e| e.to_string());
    let (width, height, step, count) = (int(plan.width)?, int(plan.height)?, int(plan.step_ms)?, int(plan.count)?);
    helpers::with_env(|env| {
        let class = helpers::find_app_class(env, "dev.gpui.beeper.VideoFrame")?;
        let path = env.new_string(path.to_string_lossy()).map_err(|e| e.to_string())?;
        let bytes = env
            .call_static_method(
                &class,
                jni::jni_str!("frames"),
                jni::jni_sig!("(Ljava/lang/String;IIII)[B"),
                &[JValue::Object(&path), JValue::Int(width), JValue::Int(height), JValue::Int(step), JValue::Int(count)],
            )
            .and_then(|v| v.l())
            .map_err(|e| crate::java_error(env, e))?;
        if bytes.is_null() {
            return Ok(Vec::new());
        }
        let bytes = env.cast_local::<JByteArray>(bytes).map_err(|e| e.to_string())?;
        env.convert_byte_array(&bytes).map_err(|e| e.to_string())
    })
}

#[cfg(target_os = "linux")]
fn grab(path: &Path, plan: Plan) -> Result<Vec<u8>, String> {
    use gstreamer as gst;
    use gstreamer::prelude::*;

    gst::init().map_err(|e| e.to_string())?;
    // Frames `step_ms` apart at the plan's size, whatever the clip's own rate; `sync=false` takes
    // them as fast as they decode.
    let description = format!(
        "filesrc name=src ! decodebin ! videoconvert ! videoscale ! videorate ! video/x-raw,format=RGBA,width={},height={},pixel-aspect-ratio=1/1,framerate=1000/{} ! appsink name=sink sync=false max-buffers=2",
        plan.width,
        plan.height,
        plan.step_ms.max(1),
    );
    let pipeline = gst::parse::launch(&description).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>().map_err(|_| "not a pipeline".to_owned())?;
    let (Some(src), Some(sink)) = (pipeline.by_name("src"), pipeline.by_name("sink")) else { return Err("pipeline without its ends".into()) };
    src.set_property("location", path.to_string_lossy().as_ref());
    pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(plan.frame_bytes() * plan.count as usize);
    // A clip that does not decode (a codec the system lacks) gives nothing within the wait.
    let wait = gst::ClockTime::from_seconds(2).nseconds();
    for _ in 0..plan.count {
        let Some(sample) = sink.emit_by_name::<Option<gst::Sample>>("try-pull-sample", &[&wait]) else { break };
        let Some(buffer) = sample.buffer() else { break };
        let Ok(map) = buffer.map_readable() else { break };
        // RGBA rows need no padding, so a frame is exactly its pixels.
        if map.len() != plan.frame_bytes() {
            break;
        }
        out.extend_from_slice(&map);
    }
    let _ = pipeline.set_state(gst::State::Null);
    Ok(out)
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn grab(_: &Path, _: Plan) -> Result<Vec<u8>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::{Plan, split};

    #[test]
    fn bytes_cut_into_whole_frames() {
        let plan = Plan { width: 2, height: 1, step_ms: 100, count: 2 };
        let frames = split((0..20).collect(), plan);
        assert_eq!(frames.len(), 2, "a partial and a third frame are dropped");
        assert_eq!(frames[1].as_raw(), &(8..16).collect::<Vec<u8>>());
        assert!(split(Vec::new(), Plan { width: 0, ..plan }).is_empty());
    }
}
