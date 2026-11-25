//! Videos play in the app's own full-screen player: on Android `dev.gpui.beeper.Player`, on Linux
//! `linux.rs` over the window. macOS and Windows have no player of their own and hand them to the
//! system's app.

use std::path::Path;

use gpui_kit::{AnyView, App, WeakEntity};

use crate::backend::{Media, MediaKind};

#[cfg(target_os = "linux")]
mod linux;

/// Opens a downloaded video in a player drawn over the window; None when this is not a video or
/// the platform draws none (Android starts its own, through `play`).
pub(super) fn open(path: &Path, media: &Media, app: WeakEntity<super::BeeperApp>, cx: &mut App) -> Option<AnyView> {
    if media.kind != MediaKind::Video {
        return None;
    }
    #[cfg(target_os = "linux")]
    return linux::open(path, media, app, cx);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, app, cx);
        None
    }
}

/// Opens a downloaded video in the in-app player. False when this is not a video, the platform
/// has no player, or it could not start: the caller then hands the file on.
pub(super) fn play(path: &Path, media: &Media) -> bool {
    if media.kind != MediaKind::Video {
        return false;
    }
    #[cfg(target_os = "android")]
    {
        let mime = media.mime.as_deref().unwrap_or("video/*");
        player(&path.to_string_lossy(), mime, media.looped).unwrap_or_else(|e| {
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
fn player(path: &str, mime: &str, looped: bool) -> Result<bool, String> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    helpers::with_env(|env| {
        let activity = helpers::activity(env)?;
        let class = helpers::find_app_class(env, "dev.gpui.beeper.Player")?;
        let path = env.new_string(path).map_err(|e| e.to_string())?;
        let mime = env.new_string(mime).map_err(|e| e.to_string())?;
        env.call_static_method(
            &class,
            jni::jni_str!("play"),
            jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;Z)Z"),
            &[
                JValue::Object(&activity),
                JValue::Object(&path),
                JValue::Object(&mime),
                JValue::Bool(looped),
            ],
        )
        .and_then(|v| v.z())
        .map_err(|e| crate::java_error(env, e))
    })
}
