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
