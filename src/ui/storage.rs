//! Settings, Data and storage, after Signal's: what the account takes on this device and a button
//! that empties the media cache, and how long messages stay in this device's caches. The sizes
//! are read when the page opens (`backend/storage.rs`); nothing here changes the server.
//! The settings file's export and import, and settings from another device, sit here too
//! (`portable.rs`).

use super::panels::{card, footnote, option, row, section};
use super::*;
use crate::backend::storage::{Keep, Usage};
use crate::i18n::t;

/// The media settings kept in `settings.json`, on this device only.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MediaSettings {
    #[serde(default)]
    pub keep: Keep,
    #[serde(default)]
    pub download: super::download::Download,
}

/// What the page knows of the shown account's storage.
#[derive(Default)]
pub(super) struct Storage {
    usage: Option<Usage>,
    /// A size or a clean-up asked for and not answered yet.
    asked: bool,
    pub(super) held: super::download::Held,
    /// What the last settings export or import came to, for the user to read.
    pub(super) moved: Option<SharedString>,
    pub(super) sync: super::portable::Sync,
}

fn keep_label(keep: Keep) -> &'static str {
    match keep {
        Keep::Forever => t!("Forever"),
        Keep::Year => t!("1 year"),
        Keep::Month => t!("30 days"),
    }
}

impl BeeperApp {
    pub(super) fn storage_summary(&self) -> SharedString {
        match self.storage.usage {
            Some(usage) => t!("{size} on this device", size = file_size(usage.total())).into(),
            None => t!("Storage, kept messages").into(),
        }
    }

    pub(super) fn storage_usage(&mut self, usage: Usage) {
        (self.storage.usage, self.storage.asked) = (Some(usage), false);
    }

    /// Asks again what the shown account takes, for the Settings list's summary and this page.
    pub(super) fn refresh_storage(&mut self, cx: &mut Context<Self>) {
        (self.storage.usage, self.storage.asked) = (None, false);
        self.ask_storage(Cmd::Storage, cx);
    }

    fn ask_storage(&mut self, cmd: Cmd, cx: &mut Context<Self>) {
        if !self.storage.asked && self.cmd.try_send(cmd).is_ok() {
            self.storage.asked = true;
            cx.notify();
        }
    }

    /// Applies the kept time to every account's caches.
    pub(super) fn set_keep(&mut self, keep: Keep, cx: &mut Context<Self>) {
        self.settings.media.keep = keep;
        self.settings.save(&self.dir);
        self.accounts.tell_all(|| Cmd::Keep(keep));
        cx.notify();
    }

    pub(super) fn render_storage(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let size = |bytes: Option<u64>| bytes.map_or_else(|| "…".to_owned(), file_size);
        let usage = self.storage.usage;
        let part = |label: &str, bytes: Option<u64>, cx: &App| {
            row(label, "", cx).child(div().flex_none().text_size(fs(15.)).font_features(tabular()).text_color(p.muted).child(size(bytes)))
        };
        let busy = self.storage.asked;
        let clear = super::panels::link("clear-media", t!("Clear media cache"), "".into(), Some(IconName::Trash), None, cx)
            .when(busy, |el| el.opacity(0.5))
            .when(!busy, |el| el.on_click(cx.listener(|this, _, _, cx| this.ask_storage(Cmd::ClearMedia, cx))));
        let keep = self.settings.media.keep;
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                section(t!("Storage"), cx)
                    .child(
                        card(cx)
                            .child(part(t!("Total"), usage.map(|u| u.total()), cx))
                            .child(part(t!("Media"), usage.map(|u| u.media), cx))
                            .child(part(t!("Messages"), usage.map(|u| u.messages), cx))
                            .child(part(t!("Search index"), usage.map(|u| u.search), cx))
                            .child(part(t!("Opened files"), usage.map(|u| u.downloads), cx))
                            .child(part(t!("Account and keys"), usage.map(|u| u.account), cx)),
                    )
                    .child(card(cx).child(clear))
                    .child(footnote(t!("Clearing removes the photos, videos and files kept on this device. They download again when shown. Nothing is deleted from the server."), cx)),
            )
            .child(self.render_download_section(cx))
            .child(self.render_portable(cx))
            .child(self.render_sync(cx))
            .child(
                section(t!("Keep messages"), cx)
                    .child(card(cx).children(Keep::ALL.into_iter().enumerate().map(|(ix, choice)| {
                        let on = keep == choice;
                        option(("keep", ix), keep_label(choice), "", on, cx).when(!on, |el| el.on_click(cx.listener(move |this, _, _, cx| this.set_keep(choice, cx))))
                    })))
                    .child(footnote(t!("Older messages leave this device's search index, and media not opened for that long leaves its cache. The server keeps everything."), cx)),
            )
            .into_any_element()
    }

    /// Export and import of the settings, with the confirmation an import waits for.
    fn render_portable(&mut self, cx: &mut Context<Self>) -> Div {
        let link = |id: &'static str, label: &'static str, icon: IconName, cx: &App| super::panels::link(id, label, "".into(), Some(icon), None, cx);
        let actions = if self.pending_settings.is_some() && !self.storage.sync.arrived {
            self.confirm_settings(t!("Replace your settings with the imported ones?"), cx)
        } else {
            card(cx)
                .child(link("settings-export", t!("Export settings"), IconName::ArrowUp, cx).on_click(cx.listener(|this, _, _, cx| this.export_settings(cx))))
                .child(link("settings-import", t!("Import settings"), IconName::ArrowDown, cx).on_click(cx.listener(|this, _, _, cx| this.import_settings(cx))))
        };
        section(t!("Settings file"), cx)
            .child(actions)
            .when_some(self.storage.moved.clone(), |el, text| el.child(footnote(&text, cx)))
            .child(footnote(t!("The file holds this app's settings for moving them to another device. The app lock, your own emoji font, sounds and photos stay on each device. Settings for one chat name it by its Matrix room id; no messages, keys or passwords go into the file."), cx))
    }
}
