//! Settings that move between devices: exported to a file and imported from one, or handed over
//! by another device of the account (`backend/handoff.rs`), through the server or the local
//! network. What only makes sense on this device stays as it is: the app lock (its PIN lives
//! here), the own emoji font, own sounds and wallpaper photos (files here), the accounts' slots
//! and staying connected.

use super::*;
use crate::Settings;
use crate::backend::handoff::Step;
use crate::backend::verify::Trust;
use crate::i18n::t;
use crate::tones::Tone;
use appearance::Wallpaper;

/// What an exported file says it is, and its layout's version.
const KIND: &str = "unisend-settings";
const VERSION: u32 = 1;
/// The most of a file that is read; an exported one is a few KB.
const MAX_FILE: u64 = 1 << 20;
/// The exported file's name.
pub(super) const FILE_NAME: &str = "unisend-settings.json";

/// Settings coming from another device of the account.
#[derive(Default)]
pub(super) struct Sync {
    /// The list of devices to ask is open.
    picking: bool,
    /// The device asked through the server, by name, until it answers.
    asking: Option<String>,
    /// The local-network exchange, from its first step until closed.
    pub(super) lan: Option<Step>,
    /// The settings waiting for confirmation came from a device, not a file: they are confirmed here.
    pub(super) arrived: bool,
    /// The downloadable emoji font the waiting settings chose, by its place in `emoji::SETS`, when
    /// this device uses another; offered once they are applied, since the font is not in them.
    emoji_with: Option<usize>,
    /// That font, offered for download.
    emoji_offer: Option<usize>,
}

/// What the list of devices below it is for.
fn choose_heading(cx: &App) -> Div {
    div().pt(px(8.)).text_size(fs(13.)).text_color(Palette::of(cx).muted).child(t!("Choose the device to take the settings from:"))
}

/// A device to take the settings from: a button with the device's icon, so it reads as one.
fn choice(id: impl Into<ElementId>, label: &str, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .min_h(px(super::panels::ROW_H))
        .py(px(8.))
        .mx(px(-8.))
        .px(px(8.))
        .gap(px(GAP))
        .rounded(px(12.))
        .role(Role::Button)
        .aria_label(label.to_owned())
        .cursor_pointer()
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
        .active(|el| el.bg(p.raised))
        .child(Icon::new(IconName::MonitorSmartphone).size_5().flex_none().text_color(p.brand))
        .child(div().flex_1().min_w_0().text_size(fs(16.)).line_height(fs(22.)).child(label.to_owned()))
        .child(Icon::new(IconName::ChevronRight).size_4().flex_none().text_color(p.faint))
}

/// The downloadable font `incoming` uses, when `here` uses a different one.
fn emoji_to_offer(here: &Settings, incoming: &Settings) -> Option<usize> {
    let family = incoming.emoji_font.as_deref().filter(|f| here.emoji_font.as_deref() != Some(*f))?;
    crate::emoji::SETS.iter().position(|set| set.family == family).filter(|_| crate::emoji::SUPPORTED)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Exported {
    kind: String,
    version: u32,
    settings: Settings,
}

/// The settings as an exported file.
pub(super) fn to_file(settings: &Settings) -> Vec<u8> {
    let file = Exported { kind: KIND.to_owned(), version: VERSION, settings: settings.clone() };
    serde_json::to_vec_pretty(&file).unwrap_or_default()
}

/// The settings an exported file holds; the error is for the user to read.
pub(super) fn from_file(raw: &[u8]) -> Result<Settings, &'static str> {
    let file: Exported = serde_json::from_slice(raw).map_err(|_| t!("This is not a Unisend settings file."))?;
    if file.kind != KIND {
        return Err(t!("This is not a Unisend settings file."));
    }
    if file.version > VERSION {
        return Err(t!("These settings come from a newer Unisend. Update the app first."));
    }
    Ok(file.settings)
}

/// `incoming` with what belongs to this device taken from `here`.
pub(super) fn adopt(here: &Settings, mut incoming: Settings) -> Settings {
    incoming.lock = here.lock.clone();
    incoming.emoji_font = here.emoji_font.clone();
    incoming.account = here.account;
    incoming.shared_inbox = here.shared_inbox.clone();
    incoming.mute_until_added = here.mute_until_added.clone();
    incoming.stay_connected = here.stay_connected;
    if incoming.wallpaper == Wallpaper::Photo {
        incoming.wallpaper = here.wallpaper;
    }
    // A chat's photo is a file on the other device: this device's look for the chat stays.
    for (room, look) in &mut incoming.chat_looks {
        if look.wallpaper == Some(Wallpaper::Photo) {
            let mine = here.chat_looks.get(room);
            (look.wallpaper, look.photo) = (mine.and_then(|l| l.wallpaper), mine.and_then(|l| l.photo.clone()));
        }
    }
    let own = &here.sounds.own;
    let keep = |tone: &mut Tone, mine: &Tone| {
        if matches!(tone, Tone::Own(file) if !own.contains(file)) {
            *tone = mine.clone();
        }
    };
    keep(&mut incoming.sounds.direct, &here.sounds.direct);
    keep(&mut incoming.sounds.group, &here.sounds.group);
    keep(&mut incoming.sounds.mention, &here.sounds.mention);
    incoming.sounds.own = own.clone();
    incoming
}

impl BeeperApp {
    /// Writes the settings to a file in Downloads (Download/Unisend on Android).
    pub(super) fn export_settings(&mut self, cx: &mut Context<Self>) {
        self.storage.moved = None;
        let raw = to_file(&self.settings);
        let tmp = self.dir.join(FILE_NAME);
        let saving = cx.background_spawn(async move {
            std::fs::write(&tmp, raw).map_err(|e| e.to_string())?;
            let saved = crate::save_file(&tmp, FILE_NAME, Some("application/json"));
            let _ = std::fs::remove_file(&tmp);
            saved
        });
        cx.spawn(async move |this, cx| {
            let done = saving.await;
            let _ = this.update(cx, |this, cx| {
                this.storage.moved = Some(match done {
                    Ok(place) => t!("Saved to {place}", place = place).into(),
                    Err(e) => {
                        log::warn!("export settings: {e}");
                        t!("Could not save the file.").into()
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Takes the settings of a file the user picks.
    pub(super) fn import_settings(&mut self, cx: &mut Context<Self>) {
        let picked = crate::pick_file(cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let read = cx.background_spawn(async move {
                use std::io::Read as _;
                let mut raw = Vec::new();
                std::fs::File::open(&path).and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut raw)).map(|_| raw)
            });
            let raw = read.await;
            let _ = this.update(cx, |this, cx| {
                let incoming = match raw {
                    Ok(raw) if raw.len() as u64 > MAX_FILE => Err(t!("This is not a Unisend settings file.")),
                    Ok(raw) => from_file(&raw),
                    Err(e) => {
                        log::warn!("import settings: {e}");
                        Err(t!("Could not read the file."))
                    }
                };
                match incoming {
                    Ok(incoming) => {
                        this.storage.sync.emoji_with = emoji_to_offer(&this.settings, &incoming);
                        let merged = adopt(&this.settings, incoming);
                        (this.pending_settings, this.storage.moved, this.storage.sync.arrived) = (Some(merged), None, false);
                    }
                    Err(e) => this.storage.moved = Some(e.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings another device sent: they wait for the same confirmation as an imported file.
    pub(super) fn settings_arrived(&mut self, raw: &[u8]) {
        let sync = &mut self.storage.sync;
        (sync.asking, sync.lan, sync.picking, sync.arrived) = (None, None, false, true);
        match from_file(raw) {
            Ok(incoming) => {
                self.storage.sync.emoji_with = emoji_to_offer(&self.settings, &incoming);
                (self.pending_settings, self.storage.moved) = (Some(adopt(&self.settings, incoming)), None);
            }
            Err(e) => self.storage.moved = Some(e.into()),
        }
    }

    /// The question before settings from a file or a device replace these, with its answers.
    pub(super) fn confirm_settings(&self, question: &'static str, cx: &mut Context<Self>) -> Div {
        let link = |id: &'static str, label: &'static str, icon: IconName, cx: &App| super::panels::link(id, label, "".into(), Some(icon), None, cx);
        super::panels::card(cx)
            .child(super::panels::row(question, "", cx))
            .child(link("settings-import-yes", t!("Replace"), IconName::Check, cx).on_click(cx.listener(|this, _, window, cx| {
                if let Some(new) = this.pending_settings.take() {
                    this.apply_settings(new, window, cx);
                    this.storage.sync.emoji_offer = this.storage.sync.emoji_with.take();
                }
            })))
            .child(link("settings-import-no", t!("Cancel"), IconName::Close, cx).on_click(cx.listener(|this, _, _, cx| {
                (this.pending_settings, this.storage.sync.emoji_with) = (None, None);
                cx.notify();
            })))
    }

    /// Asking another device, through the server or on the local network, and the steps between.
    pub(super) fn render_sync(&mut self, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let action = |id: &'static str, label: &'static str, icon: IconName, cx: &App| super::panels::link(id, label, "".into(), Some(icon), None, cx);
        let cancel_lan = |cx: &mut Context<Self>| {
            action("sync-lan-cancel", t!("Cancel"), IconName::Close, cx).on_click(cx.listener(|this, _, _, cx| {
                let _ = this.cmd.try_send(Cmd::HandoffStop);
                this.storage.sync.lan = None;
                cx.notify();
            }))
        };
        let sync = &self.storage.sync;
        let card = super::panels::card(cx);
        let card = if sync.arrived && self.pending_settings.is_some() {
            self.confirm_settings(t!("Replace your settings with the ones from your other device?"), cx)
        } else if let Some(n) = sync.emoji_offer {
            let set = &crate::emoji::SETS[n];
            let busy = self.emoji.busy() || self.emoji.pending;
            let value = match &self.emoji.error {
                _ if busy => self.emoji_value(),
                Some(error) => error.clone(),
                None => format!("{}, {}", set.name, file_size(set.size)),
            };
            card.child(
                super::panels::link("sync-emoji", t!("Download the other device's emoji"), value.into(), Some(IconName::Download), None, cx)
                    .when(!busy, |el| el.on_click(cx.listener(move |this, _, _, cx| this.download_emoji_set(n, cx)))),
            )
            .child(action("sync-emoji-close", t!("Close"), IconName::Close, cx).on_click(cx.listener(|this, _, _, cx| {
                this.storage.sync.emoji_offer = None;
                cx.notify();
            })))
            .child(div().pb(px(8.)).text_size(fs(13.)).text_color(p.muted).child(t!("A font is downloaded from GitHub only when tapped: GitHub sees the device's IP address and which font.")))
        } else if let Some(step) = sync.lan.clone() {
            match step {
                Step::Offering => card.child(super::panels::row(t!("Waiting for the other device"), t!("On it, open Data and storage and choose Get over local network."), cx)).child(cancel_lan(cx)),
                Step::Seeking(found) if found.is_empty() => card.child(super::panels::row(t!("Looking for a device"), t!("On the other device, choose Send over local network."), cx)).child(cancel_lan(cx)),
                Step::Seeking(found) => card
                    .child(choose_heading(cx))
                    .children(found.into_iter().enumerate().map(|(ix, at)| {
                        choice(("sync-peer", ix), &t!("Unisend at {address}", address = at), cx).on_click(cx.listener(move |this, _, _, cx| {
                            let _ = this.cmd.try_send(Cmd::HandoffPick(ix));
                            cx.notify();
                        }))
                    }))
                    .child(cancel_lan(cx)),
                Step::Code { code, offering } => {
                    let shown = div().py(px(8.)).text_size(fs(32.)).font_features(tabular()).text_color(p.text).child(code);
                    let say = if offering { t!("Check that the other device shows the same number, then send.") } else { t!("Check that the other device shows the same number, and send from there.") };
                    card.child(shown)
                        .child(div().pb(px(8.)).text_size(fs(13.)).text_color(p.muted).child(say))
                        .when(offering, |el| {
                            el.child(action("sync-lan-send", t!("Send"), IconName::Check, cx).on_click(cx.listener(|this, _, _, cx| {
                                let _ = this.cmd.try_send(Cmd::HandoffConfirm);
                                cx.notify();
                            })))
                        })
                        .child(cancel_lan(cx))
                }
                Step::Sent | Step::Failed(_) => {
                    let said = match step {
                        Step::Failed(e) => e,
                        _ => t!("Settings sent.").to_owned(),
                    };
                    card.child(super::panels::row(&said, "", cx)).child(action("sync-lan-done", t!("Close"), IconName::Check, cx).on_click(cx.listener(|this, _, _, cx| {
                        this.storage.sync.lan = None;
                        cx.notify();
                    })))
                }
            }
        } else if let Some(name) = sync.asking.clone() {
            card.child(super::panels::row(&t!("Waiting for {device}", device = name), t!("It needs Unisend running and online."), cx)).child(
                action("sync-ask-cancel", t!("Cancel"), IconName::Close, cx).on_click(cx.listener(|this, _, _, cx| {
                    let _ = this.cmd.try_send(Cmd::CancelAsk);
                    this.storage.sync.asking = None;
                    cx.notify();
                })),
            )
        } else if sync.picking {
            let devices = self.verify.devices.as_ref();
            let this_verified = devices.is_some_and(|d| d.this_verified);
            let others: Vec<(String, String)> = devices
                .map(|d| d.list.iter().filter(|d| !d.this && d.trust == Trust::Verified).map(|d| (d.id.clone(), d.name.clone())).collect())
                .unwrap_or_default();
            let card = if !this_verified {
                card.child(super::panels::row(t!("Verify this device first, in Settings, Devices."), "", cx))
            } else if others.is_empty() {
                card.child(super::panels::row(t!("No other verified device of this account."), "", cx))
            } else {
                card.child(choose_heading(cx)).children(others.into_iter().enumerate().map(|(ix, (id, name))| {
                    choice(("sync-device", ix), &name, cx).on_click(cx.listener(move |this, _, _, cx| {
                        if this.send(Cmd::AskSettings(id.clone()), cx) {
                            (this.storage.sync.asking, this.storage.sync.picking) = (Some(name.clone()), false);
                        }
                        cx.notify();
                    }))
                }))
            };
            card.child(action("sync-pick-cancel", t!("Cancel"), IconName::Close, cx).on_click(cx.listener(|this, _, _, cx| {
                this.storage.sync.picking = false;
                cx.notify();
            })))
        } else {
            card.child(action("sync-ask", t!("Get from another device"), IconName::ArrowDown, cx).on_click(cx.listener(|this, _, _, cx| {
                this.storage.sync.picking = true;
                cx.notify();
            })))
            .child(action("sync-lan-get", t!("Get over local network"), IconName::ArrowDown, cx).on_click(cx.listener(|this, _, _, cx| {
                if this.send(Cmd::HandoffSeek, cx) {
                    this.storage.sync.lan = Some(Step::Seeking(Vec::new()));
                }
                cx.notify();
            })))
            .child(action("sync-lan-offer", t!("Send over local network"), IconName::ArrowUp, cx).on_click(cx.listener(|this, _, _, cx| {
                if this.send(Cmd::HandoffOffer(to_file(&this.settings)), cx) {
                    this.storage.sync.lan = Some(Step::Offering);
                }
                cx.notify();
            })))
        };
        super::panels::section(t!("Sync with another device"), cx).child(card).child(super::panels::footnote(
            t!("Get from another device: the settings come encrypted from one of your verified devices, through your server, which sees only that two of your devices exchanged a message. Over the local network no server takes part: both devices are on the same network with this page open, and you compare a number on both screens."),
            cx,
        ))
    }

    /// Puts `new` in place of the settings, with each one's effect, as its own control would.
    pub(super) fn apply_settings(&mut self, new: Settings, window: &mut Window, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.settings, new);
        self.settings.save(&self.dir);
        let s = &self.settings;
        cx.set_global(s.theme);
        cx.set_global(s.chat_colour);
        crate::sync_theme(Some(window), cx);
        appearance::SCALE.store(s.text_size.percent(), std::sync::atomic::Ordering::Relaxed);
        s.notify_shows.apply();
        s.sounds.apply();
        crate::quiet::apply(&s.chat_notify, &s.profiles);
        crate::ime::set_incognito(s.incognito_keyboard);
        let (incognito, reduce_motion, link_previews, language) = (s.incognito, s.reduce_motion, s.link_previews, s.language);
        if incognito != old.incognito {
            self.accounts.tell_all(|| Cmd::Incognito(incognito));
        }
        if reduce_motion != old.reduce_motion {
            if reduce_motion {
                self.system_reduce_motion = cx.reduce_motion();
                cx.set_reduce_motion(true);
            } else {
                cx.set_reduce_motion(self.system_reduce_motion);
                motion::follow_system(cx);
                self.system_reduce_motion = cx.reduce_motion();
            }
        }
        if !link_previews {
            self.close_link(false, cx);
        }
        if language != old.language {
            crate::set_platform_language(language);
            let before = crate::i18n::polish();
            crate::i18n::set(language);
            if crate::i18n::polish() != before {
                self.relabel(window, cx);
            }
        }
        self.timeline.remeasure();
        self.storage.moved = Some(t!("Settings imported.").into());
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{Settings, Tone, Wallpaper, adopt, emoji_to_offer, from_file, to_file};

    fn settings() -> Settings {
        from_file(&to_file(&serde_json::from_str::<Settings>("{}").unwrap())).unwrap()
    }

    #[test]
    fn a_file_comes_back_as_it_went_and_others_are_refused() {
        let mut s = settings();
        s.enter_sends = false;
        assert_eq!(from_file(&to_file(&s)).unwrap(), s);
        assert!(from_file(b"{}").is_err());
        assert!(from_file(br#"{"kind":"other","version":1,"settings":{}}"#).is_err());
        assert!(from_file(br#"{"kind":"unisend-settings","version":99,"settings":{}}"#).unwrap_err().contains("newer"));
    }

    #[test]
    fn what_belongs_to_this_device_stays() {
        let mut here = settings();
        here.account = 2;
        here.emoji_font = Some("Mine".into());
        here.lock.timeout = 77;
        here.sounds.own = vec!["a.ogg".into()];
        here.sounds.direct = Tone::Own("a.ogg".into());
        let mut there = settings();
        there.enter_sends = false;
        there.wallpaper = Wallpaper::Photo;
        there.sounds.direct = Tone::Own("b.ogg".into());
        there.sounds.group = Tone::Own("a.ogg".into());
        let got = adopt(&here, there);
        assert!(!got.enter_sends);
        assert_eq!((got.account, got.emoji_font.as_deref(), got.lock.timeout), (2, Some("Mine"), 77));
        assert_eq!(got.wallpaper, here.wallpaper);
        // Their own sound is not here; one with the same file is.
        assert_eq!((got.sounds.direct, got.sounds.group), (Tone::Own("a.ogg".into()), Tone::Own("a.ogg".into())));
    }

    #[test]
    fn only_a_downloadable_emoji_font_this_device_lacks_is_offered() {
        let (mut here, mut there) = (settings(), settings());
        assert_eq!(emoji_to_offer(&here, &there), None);
        there.emoji_font = Some("Twemoji Mozilla".into());
        assert_eq!(emoji_to_offer(&here, &there), crate::emoji::SUPPORTED.then_some(1));
        here.emoji_font = Some("Twemoji Mozilla".into());
        assert_eq!(emoji_to_offer(&here, &there), None);
        there.emoji_font = Some("A file of their own".into());
        assert_eq!(emoji_to_offer(&here, &there), None);
    }
}
