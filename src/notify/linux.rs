//! `org.freedesktop.Notifications` on the session bus (spec 1.2; KDE adds the reply box).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt as _;
use zbus::zvariant::Value;

use super::{APP_ID, Act, Entry, Event, MAX_CHATS, Target, act, event, file_uri};
use crate::i18n::t;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// The numbers of the notifications this app has showing. A server sends its signals to every
/// listener: other apps' would otherwise fill the button queue.
static OURS: Mutex<VecDeque<u32>> = Mutex::new(VecDeque::new());
/// The token of the notification about to be clicked open, until the click hands it to the raise.
static TOKEN: Mutex<Option<(u32, String)>> = Mutex::new(None);

fn ours(id: u32) -> bool {
    OURS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&id)
}

fn forget(id: u32) {
    OURS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|&n| n != id);
}

/// Lets the raise that follows a click pass KWin's focus stealing prevention.
pub(super) fn hand_token() {
    if let Some((_, token)) = TOKEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
        gpui_linux::set_next_activation_token(token);
    }
}

pub(super) struct Sink {
    proxy: zbus::Proxy<'static>,
    markup: bool,
}

impl Sink {
    /// One connection for the app's lifetime, once the server answers.
    pub(super) async fn open() -> Result<Self, String> {
        let proxy = match tokio::time::timeout(CALL_TIMEOUT, connect()).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => return Err("the session bus did not answer".into()),
        };
        let caps: Vec<String> = proxy.call("GetCapabilities", &()).await.map_err(|e| e.to_string())?;
        match proxy.receive_all_signals().await {
            // Ends with the connection, or with the backend's runtime.
            Ok(signals) => drop(tokio::spawn(signals.for_each(|msg| async move { on_signal(&msg) }))),
            Err(e) => log::warn!("desktop notification buttons: {e}"),
        }
        Ok(Self { proxy, markup: caps.iter().any(|c| c == "body-markup") })
    }

    pub(super) fn markup(&self) -> bool {
        self.markup
    }

    pub(super) async fn post(&mut self, entry: &mut Entry, body: &str, alert: bool, buttons: bool) {
        // Plasma keeps a replaced notification's state: once its popup has timed out, a
        // replacement updates only the history. A new message gets a new notification.
        if alert && entry.id != 0 {
            self.close(entry).await;
            entry.id = 0;
        }
        let all = ["default", t!("Open"), "inline-reply", t!("Reply"), "read", t!("Mark as read"), "mute", t!("Mute")];
        let actions = if buttons { &all[..] } else { &all[..2] };
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", APP_ID.into());
        hints.insert("category", "im.received".into());
        hints.insert("urgency", 1u8.into());
        hints.insert("x-kde-reply-placeholder-text", t!("Reply").into());
        hints.insert("x-kde-reply-submit-button-text", t!("Send").into());
        if !entry.icon.is_empty() {
            hints.insert("image-path", file_uri(&entry.icon).into());
        }
        if alert {
            hints.insert("sound-name", "message-new-instant".into());
        } else {
            hints.insert("suppress-sound", true.into());
        }
        let args = ("Unisend", entry.id, APP_ID, entry.name.as_str(), body, actions, hints, -1i32);
        match self.proxy.call("Notify", &args).await {
            Ok(id) => {
                if id != entry.id {
                    forget(entry.id);
                    let mut ours = OURS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    // Closed ones are forgotten as they go; this only bounds a server that never says.
                    if ours.len() == 2 * MAX_CHATS {
                        ours.pop_front();
                    }
                    ours.push_back(id);
                }
                entry.id = id;
            }
            Err(e) => log::warn!("notification: {e}"),
        }
    }

    pub(super) async fn close(&mut self, entry: &Entry) {
        forget(entry.id);
        if entry.id != 0
            && let Err(e) = self.proxy.call::<_, _, ()>("CloseNotification", &(entry.id,)).await
        {
            log::debug!("closing a notification: {e}");
        }
    }
}

/// A chat's picture as a circle: servers show `image-path` as it is, where Android and Windows
/// crop it round. Pictures come as small squares (`backend::square`); anything that does not
/// decode is passed on as it came.
pub(crate) fn round(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(picture) = image::load_from_memory(&bytes) else { return bytes };
    let mut picture = picture.to_rgba8();
    let (w, h) = picture.dimensions();
    let (cx, cy, r) = (w as f32 / 2.0, h as f32 / 2.0, w.min(h) as f32 / 2.0);
    for (x, y, px) in picture.enumerate_pixels_mut() {
        let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
        // One pixel of soft edge.
        let cover = (r - d + 0.5).clamp(0.0, 1.0);
        px[3] = (f32::from(px[3]) * cover).round() as u8;
    }
    let mut out = Vec::new();
    match picture.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png) {
        Ok(()) => out,
        Err(_) => bytes,
    }
}

async fn connect() -> zbus::Result<zbus::Proxy<'static>> {
    let conn = zbus::connection::Builder::session()?.method_timeout(CALL_TIMEOUT).build().await?;
    zbus::proxy::Builder::new(&conn)
        .destination("org.freedesktop.Notifications")?
        .path("/org/freedesktop/Notifications")?
        .interface("org.freedesktop.Notifications")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

fn on_signal(msg: &zbus::Message) {
    let header = msg.header();
    let body = msg.body();
    match header.member().map(|m| m.as_str()) {
        Some("ActionInvoked") => {
            if let Ok((id, key)) = body.deserialize::<(u32, &str)>()
                && ours(id)
                && let Some(act) = act(key)
            {
                // Kept only for the click that opens the window.
                let mut token = TOKEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if act != Act::Open || token.as_ref().is_some_and(|(of, _)| *of != id) {
                    *token = None;
                }
                drop(token);
                event(Event::Act(Target::Id(id), act));
            }
        }
        // Sent just before ActionInvoked for the same click.
        Some("ActivationToken") => {
            let token = body.deserialize::<(u32, String)>().ok().filter(|(id, _)| ours(*id));
            *TOKEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = token;
        }
        Some("NotificationReplied") => {
            if let Ok((id, text)) = body.deserialize::<(u32, String)>()
                && ours(id)
            {
                event(Event::Replied(Target::Id(id), text));
            }
        }
        Some("NotificationClosed") => {
            if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>()
                && ours(id)
            {
                forget(id);
                event(Event::Closed(Target::Id(id)));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_picture_turns_round_with_clear_corners() {
        let square = image::RgbImage::from_pixel(16, 16, image::Rgb([200, 40, 40]));
        let mut png = Vec::new();
        square.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let round = image::load_from_memory(&super::round(png)).unwrap().to_rgba8();
        assert_eq!(round.get_pixel(0, 0)[3], 0, "a corner is clear");
        assert_eq!(*round.get_pixel(8, 8), image::Rgba([200, 40, 40, 255]), "the middle stays");
        assert!(round.get_pixel(0, 8)[3] > 240, "the circle reaches the edge");
        assert_eq!(super::round(b"not a picture".to_vec()), b"not a picture");
    }
}
