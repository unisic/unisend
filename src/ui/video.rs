//! Videos play in the app's own full-screen player on Android (`dev.gpui.beeper.Player`); the
//! desktop has no player of its own and hands them to the system's app.

use std::path::Path;

use crate::backend::{Media, MediaKind};

/// Opens a downloaded video in the in-app player. False when this is not a video, the platform
/// has no player, or it could not start: the caller then hands the file on.
pub(super) fn play(path: &Path, media: &Media) -> bool {
    if media.kind != MediaKind::Video {
        return false;
    }
    #[cfg(target_os = "android")]
    {
        let (width, height) = media.dims.unwrap_or_default();
        let mime = media.mime.as_deref().unwrap_or("video/*");
        player(&path.to_string_lossy(), mime, media.looped, width, height).unwrap_or_else(|e| {
            log::warn!("video player: {e}");
            false
        })
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "android")]
fn player(path: &str, mime: &str, looped: bool, width: u32, height: u32) -> Result<bool, String> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    let side = |n: u32| i32::try_from(n).unwrap_or(0);
    helpers::with_env(|env| {
        let activity = helpers::activity(env)?;
        let class = helpers::find_app_class(env, "dev.gpui.beeper.Player")?;
        let path = env.new_string(path).map_err(|e| e.to_string())?;
        let mime = env.new_string(mime).map_err(|e| e.to_string())?;
        env.call_static_method(
            &class,
            jni::jni_str!("play"),
            jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;ZII)Z"),
            &[
                JValue::Object(&activity),
                JValue::Object(&path),
                JValue::Object(&mime),
                JValue::Bool(looped),
                JValue::Int(side(width)),
                JValue::Int(side(height)),
            ],
        )
        .and_then(|v| v.z())
        .map_err(|e| crate::java_error(env, e))
    })
}
