//! `org.freedesktop.Notifications` on the session bus (spec 1.2; KDE adds the reply box).

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use zbus::zvariant::Value;

use super::{APP_ID, Entry, Event, Target, act, event};
use crate::i18n::t;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Sink {
    proxy: zbus::Proxy<'static>,
    markup: bool,
}

impl Sink {
    /// One connection for the app's lifetime; `None`, logged once, when there is no server.
    pub(super) async fn open() -> Option<Self> {
        let proxy = match tokio::time::timeout(CALL_TIMEOUT, connect()).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                log::warn!("no desktop notifications: {e}");
                return None;
            }
            Err(_) => {
                log::warn!("no desktop notifications: the session bus did not answer");
                return None;
            }
        };
        let caps: Vec<String> = match proxy.call("GetCapabilities", &()).await {
            Ok(caps) => caps,
            Err(e) => {
                log::warn!("no desktop notifications: {e}");
                return None;
            }
        };
        match proxy.receive_all_signals().await {
            // Ends with the connection, or with the backend's runtime.
            Ok(signals) => drop(tokio::spawn(signals.for_each(|msg| async move { on_signal(&msg) }))),
            Err(e) => log::warn!("desktop notification buttons: {e}"),
        }
        Some(Self { proxy, markup: caps.iter().any(|c| c == "body-markup") })
    }

    pub(super) fn markup(&self) -> bool {
        self.markup
    }

    pub(super) async fn post(&mut self, entry: &mut Entry, body: &str, alert: bool) {
        let actions = ["default", t!("Open"), "inline-reply", t!("Reply"), "read", t!("Mark as read"), "mute", t!("Mute")];
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", APP_ID.into());
        hints.insert("category", "im.received".into());
        hints.insert("urgency", 1u8.into());
        hints.insert("x-kde-reply-placeholder-text", t!("Reply").into());
        if !entry.icon.is_empty() {
            hints.insert("image-path", file_uri(&entry.icon).into());
        }
        if alert {
            hints.insert("sound-name", "message-new-instant".into());
        } else {
            hints.insert("suppress-sound", true.into());
        }
        let args = ("Unisend", entry.id, APP_ID, entry.name.as_str(), body, &actions[..], hints, -1i32);
        match self.proxy.call("Notify", &args).await {
            Ok(id) => entry.id = id,
            Err(e) => log::warn!("notification: {e}"),
        }
    }

    pub(super) async fn close(&mut self, entry: &Entry) {
        if entry.id != 0
            && let Err(e) = self.proxy.call::<_, _, ()>("CloseNotification", &(entry.id,)).await
        {
            log::debug!("closing a notification: {e}");
        }
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
                && let Some(act) = act(key)
            {
                event(Event::Act(Target::Id(id), act));
            }
        }
        // Sent just before ActionInvoked for the same click: lets the raise that follows pass
        // KWin's focus stealing prevention.
        Some("ActivationToken") => {
            if let Ok((_id, token)) = body.deserialize::<(u32, String)>() {
                gpui_linux::set_next_activation_token(token);
            }
        }
        Some("NotificationReplied") => {
            if let Ok((id, text)) = body.deserialize::<(u32, String)>() {
                event(Event::Replied(Target::Id(id), text));
            }
        }
        Some("NotificationClosed") => {
            if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>() {
                event(Event::Closed(Target::Id(id)));
            }
        }
        _ => {}
    }
}

/// `image-path` takes a `file://` URI.
fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/._-~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_picture_path_becomes_a_file_uri() {
        assert_eq!(super::file_uri("/home/a b/notify/_x_y"), "file:///home/a%20b/notify/_x_y");
    }
}
