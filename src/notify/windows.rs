//! WinRT toasts. An unpackaged app shows them under an AppUserModelID registered in
//! `HKCU\Software\Classes\AppUserModelId`; buttons and the reply box answer while the app runs.

use std::collections::HashMap;
use std::path::Path;

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{IPropertyValue, TypedEventHandler};
use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::core::{HSTRING, IInspectable, Interface as _, h};

use super::{APP_ID, Entry, Event, Target, act, event, file_uri};
use crate::i18n::t;

/// Every chat's toast is in this group; its tag says which chat.
const GROUP: &str = "chats";

pub(super) struct Sink {
    notifier: ToastNotifier,
    /// Each chat's toast, kept so its button handler lives as long as it shows.
    toasts: HashMap<String, ToastNotification>,
}

/// Names the app and gives it its icon for toasts and the notification settings: an unpackaged
/// app has them only from its AppUserModelID's key. The icon is a file in the app's data folder.
pub(super) fn register(dir: &Path) {
    let icon = dir.join("icon.png");
    if let Err(e) = std::fs::write(&icon, include_bytes!("../../packaging/linux/icons/hicolor/256x256/apps/dev.gpui.beeper.png")) {
        log::warn!("notification icon: {e}");
    }
    let registered = windows_registry::CURRENT_USER.create(format!(r"Software\Classes\AppUserModelId\{APP_ID}")).and_then(|key| {
        key.set_string("DisplayName", "Unisend")?;
        key.set_string("IconUri", icon.to_string_lossy().as_ref())
    });
    if let Err(e) = registered {
        log::warn!("desktop notifications may not show: {e}");
    }
}

impl Sink {
    pub(super) async fn open() -> Result<Self, String> {
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID)).map_err(|e| e.to_string())?;
        let sink = Self { notifier, toasts: HashMap::new() };
        // Toasts left in the Action Center by an earlier run, whose buttons no longer answer.
        sink.clear();
        Ok(sink)
    }

    pub(super) fn markup(&self) -> bool {
        false
    }

    /// Takes every toast of the app away, also those `toasts` no longer holds.
    pub(super) fn clear(&self) {
        if let Err(e) = ToastNotificationManager::History().and_then(|history| history.ClearWithId(&HSTRING::from(APP_ID))) {
            log::debug!("clearing notifications: {e}");
        }
    }

    pub(super) async fn post(&mut self, entry: &mut Entry, body: &str, alert: bool, buttons: bool) {
        match self.show(entry, body, alert, buttons) {
            Ok(toast) => drop(self.toasts.insert(entry.chat.clone(), toast)),
            Err(e) => log::warn!("notification: {e}"),
        }
    }

    fn show(&self, entry: &Entry, body: &str, alert: bool, buttons: bool) -> windows::core::Result<ToastNotification> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(toast_xml(entry, body, alert, buttons)))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        // A later toast with the same tag and group takes this one's place.
        toast.SetTag(&HSTRING::from(tag(&entry.chat)))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        // A quiet change (an edit, the app's own line) updates the Action Center without a popup.
        toast.SetSuppressPopup(!alert)?;
        let chat = entry.chat.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, args| {
            let Some(args) = args.as_ref().and_then(|a| a.cast::<ToastActivatedEventArgs>().ok()) else { return Ok(()) };
            let key = args.Arguments()?.to_string();
            if key == "reply" {
                let text = args.UserInput()?.Lookup(h!("reply"))?.cast::<IPropertyValue>()?.GetString()?;
                event(Event::Replied(Target::Chat(chat.clone()), text.to_string()));
            } else if let Some(act) = act(&key) {
                event(Event::Act(Target::Chat(chat.clone()), act));
            }
            Ok(())
        }))?;
        self.notifier.Show(&toast)?;
        Ok(toast)
    }

    pub(super) async fn close(&mut self, entry: &Entry) {
        self.toasts.remove(&entry.chat);
        let removed = ToastNotificationManager::History().and_then(|history| {
            history.RemoveGroupedTagWithId(&HSTRING::from(tag(&entry.chat)), &HSTRING::from(GROUP), &HSTRING::from(APP_ID))
        });
        if let Err(e) = removed {
            log::debug!("closing a notification: {e}");
        }
    }
}

/// A toast's tag has at most 64 characters, fewer than a room id may have.
fn tag(chat: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    chat.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn toast_xml(entry: &Entry, body: &str, alert: bool, buttons: bool) -> String {
    let image = if entry.icon.is_empty() {
        String::new()
    } else {
        format!(r#"<image placement="appLogoOverride" hint-crop="circle" src="{}"/>"#, escape(&file_uri(&format!("/{}", entry.icon.replace('\\', "/")))))
    };
    let audio = if alert { r#"<audio src="ms-winsoundevent:Notification.IM"/>"# } else { r#"<audio silent="true"/>"# };
    let actions = if buttons {
        format!(
            concat!(
                r#"<actions><input id="reply" type="text" placeHolderContent="{reply}"/>"#,
                r#"<action content="{reply}" arguments="reply" hint-inputId="reply"/>"#,
                r#"<action content="{read}" arguments="read"/><action content="{mute}" arguments="mute"/></actions>"#,
            ),
            reply = escape(t!("Reply")),
            read = escape(t!("Mark as read")),
            mute = escape(t!("Mute")),
        )
    } else {
        String::new()
    };
    format!(
        r#"<toast launch="default"><visual><binding template="ToastGeneric"><text>{name}</text><text>{body}</text>{image}</binding></visual>{actions}{audio}</toast>"#,
        name = escape(&entry.name),
        body = escape(body),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}
