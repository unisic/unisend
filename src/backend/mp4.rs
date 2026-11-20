//! A video that cannot go as it is, made into an MP4 on the device once the user agrees: the chat's
//! bridge refuses its type (Meta takes MP4 and MOV only), or the metadata walk does not know its
//! format. The MP4 is H.264 and AAC, which every bridge takes, without metadata, and is then sent as
//! any video. Nothing leaves the device here. Desktop runs the system's ffmpeg; Android its own
//! codecs (`dev.gpui.beeper.VideoConvert`, MediaCodec), which every phone has.
//!
//! Cost: one conversion at a time (attachments are sent one by one), CPU-bound for about the
//! video's length (ffmpeg at `veryfast`; the phone's hardware encoder); the MP4 waits in `pasted/`
//! until it is sent, then goes.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{Result, bail};

use crate::i18n::t;

/// Why a video cannot go as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfit {
    /// The chat's bridge refuses its type.
    Refused,
    /// Its metadata cannot be removed.
    Unclean,
}

impl std::fmt::Display for Unfit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unfit::Refused => t!("this network does not take this video format"),
            Unfit::Unclean => t!("could not remove the location from this video"),
        })
    }
}

impl std::error::Error for Unfit {}

/// Whether a conversion can run here: always on Android; elsewhere where ffmpeg is installed, asked
/// once, blocking.
pub fn available() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| cfg!(target_os = "android") || quiet(Command::new("ffmpeg").arg("-version")).is_ok_and(|s| s.success()))
}

fn quiet(cmd: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()
}

/// Makes `source` into the MP4 `out` with the phone's codecs, blocking.
#[cfg(target_os = "android")]
pub fn convert(source: &Path, out: &Path) -> Result<()> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    let done = helpers::with_env(|env| {
        let class = helpers::find_app_class(env, "dev.gpui.beeper.VideoConvert")?;
        let source = env.new_string(source.to_string_lossy()).map_err(|e| e.to_string())?;
        let out = env.new_string(out.to_string_lossy()).map_err(|e| e.to_string())?;
        env.call_static_method(&class, jni::jni_str!("convert"), jni::jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Z"), &[JValue::Object(&source), JValue::Object(&out)])
            .and_then(|v| v.z())
            .map_err(|e| crate::java_error(env, e))
    });
    match done {
        Ok(true) => Ok(()),
        Ok(false) => bail!(t!("this phone could not convert the video")),
        Err(e) => bail!("{e}"),
    }
}

/// Makes `source` into the MP4 `out`, blocking: x264 where ffmpeg has it, else OpenH264 (Fedora's
/// own build has only that). Even sizes, as H.264 in 4:2:0 needs.
#[cfg(not(target_os = "android"))]
pub fn convert(source: &Path, out: &Path) -> Result<()> {
    let mut last = None;
    for (codec, quality) in [("libx264", ["-preset", "veryfast", "-crf", "23"]), ("libopenh264", ["-b:v", "4M", "-allow_skip_frames", "1"])] {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-loglevel", "error", "-y", "-i"]).arg(source);
        cmd.args(["-map", "0:v:0", "-map", "0:a:0?", "-map_metadata", "-1", "-map_chapters", "-1", "-sn", "-dn"]);
        cmd.args(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2", "-pix_fmt", "yuv420p", "-c:v", codec]).args(quality);
        cmd.args(["-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart", "-f", "mp4"]).arg(out);
        let status = quiet(&mut cmd)?;
        if status.success() {
            return Ok(());
        }
        last = Some(status);
    }
    bail!("ffmpeg: {}", last.map_or_else(String::new, |s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs ffmpeg: makes a WebM, then an MP4 of it that the metadata walk takes.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn a_webm_becomes_a_clean_mp4() {
        let dir = std::env::temp_dir().join(format!("mp4-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (webm, mp4) = (dir.join("in.webm"), dir.join("out.mp4"));
        let made = quiet(Command::new("ffmpeg").args(["-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=99x63:duration=1", "-metadata", "title=GPS+52", "-c:v", "libvpx-vp9"]).arg(&webm)).unwrap();
        assert!(made.success());
        convert(&webm, &mp4).unwrap();
        let mut data = std::fs::read(&mp4).unwrap();
        assert!(!data.windows(6).any(|w| w == b"GPS+52"));
        super::super::strip::video(&mut data).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
