//! Settings that move between devices: exported to a file and imported from one. What only makes
//! sense on this device stays as it is: the app lock (its PIN lives here), the own emoji font,
//! own sounds and wallpaper photos (files here), the accounts' slots and staying connected.

use super::*;
use crate::Settings;
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
                        let merged = adopt(&this.settings, incoming);
                        (this.pending_settings, this.storage.moved) = (Some(merged), None);
                    }
                    Err(e) => this.storage.moved = Some(e.into()),
                }
                cx.notify();
            });
        })
        .detach();
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
    use super::{Settings, Tone, Wallpaper, adopt, from_file, to_file};

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
}
