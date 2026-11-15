//! `UNUserNotificationCenter` through GPUI's system notifications, whose delegate the center
//! already has: it works only from the app bundle, and offers buttons but no reply box, picture or
//! sound.

use gpui_kit::{App, SystemNotification, SystemNotificationAction};

use super::{Entry, Event, Target, ToUi, act, event, to_ui};
use crate::i18n::t;

pub(super) struct Sink;

impl Sink {
    pub(super) async fn open() -> Option<Self> {
        Some(Self)
    }

    pub(super) fn markup(&self) -> bool {
        false
    }

    pub(super) async fn post(&mut self, entry: &mut Entry, body: &str, _alert: bool) {
        let action = |id: &'static str, label: &'static str| SystemNotificationAction { id: id.into(), label: label.into() };
        to_ui(ToUi::Show(SystemNotification {
            // A request with the same identifier takes the earlier one's place.
            tag: entry.chat.clone().into(),
            title: entry.name.clone().into(),
            body: body.to_owned().into(),
            actions: vec![action("read", t!("Mark as read")), action("mute", t!("Mute"))],
        }));
    }

    pub(super) async fn close(&mut self, entry: &Entry) {
        to_ui(ToUi::Dismiss(entry.chat.clone()));
    }
}

pub(super) fn attach(cx: &mut App) {
    cx.on_system_notification_response(|response, _| {
        if let Some(act) = act(response.action_id.as_deref().unwrap_or("default")) {
            event(Event::Act(Target::Chat(response.tag.to_string()), act));
        }
    });
}
