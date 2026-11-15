//! Camera and Video tiles at the start of the attach sheet's grid. Android only: they start the
//! system camera (`dev.gpui.beeper.CameraCapture`) and send what it saves through the same queue
//! as the chosen photos. Desktop has no tiles.

use super::*;
use crate::i18n::t;

/// Tiles before the first photo in the grid.
pub(super) const TILES: usize = if cfg!(target_os = "android") { 2 } else { 0 };

impl BeeperApp {
    /// Closes the sheet, runs the camera off the UI thread and queues the shot. A cancelled
    /// capture changes nothing: the reply being composed stays.
    fn capture(&mut self, video: bool, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        self.close_gallery(cx);
        cx.spawn(async move |this, cx| {
            let shot = cx.background_spawn(async move { take(video) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(path) = shot else { return };
                let reply_to = match std::mem::replace(&mut this.compose, Compose::New) {
                    Compose::Reply(id) => Some(id),
                    other => {
                        this.compose = other;
                        None
                    }
                };
                this.uploads += 1;
                this.queue_attach(room, vec![path], reply_to, cx);
            });
        })
        .detach();
    }

    pub(super) fn render_camera_tile(&mut self, ix: usize, cell: f32, live: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let video = ix == 1;
        div()
            .id(("gallery-camera", ix))
            .size(px(cell))
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .bg(p.raised)
            .text_color(p.text)
            .text_size(fs(13.))
            .cursor_pointer()
            .active(|el| el.bg(p.lifted))
            .child(Icon::new(if video { IconName::Video } else { IconName::Camera }).size_6())
            .child(if video { t!("Video") } else { t!("Photo") })
            .when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| this.capture(video, cx))))
    }
}

#[cfg(target_os = "android")]
fn take(video: bool) -> Option<PathBuf> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    let taken = helpers::with_env(|env| {
        let activity = helpers::activity(env)?;
        let class = helpers::find_app_class(env, "dev.gpui.beeper.CameraCapture")?;
        let path = env
            .call_static_method(
                &class,
                jni::jni_str!("capture"),
                jni::jni_sig!("(Landroid/app/Activity;Z)Ljava/lang/String;"),
                &[JValue::Object(&activity), JValue::Bool(video)],
            )
            .and_then(|v| v.l())
            .map_err(|e| crate::java_error(env, e))?;
        Ok((!path.is_null()).then(|| helpers::get_string(env, &path)))
    });
    taken.map_err(|e| log::warn!("camera: {e}")).ok()?.map(PathBuf::from)
}

#[cfg(not(target_os = "android"))]
fn take(_video: bool) -> Option<PathBuf> {
    None
}
