//! Nicknames and notes about people, as Signal has them: a name of the user's choosing that stands
//! for the person's own in the chat list, the chat and its info, and a note shown only in their
//! info. Both stay on this device, per account, in `people.json` (sealed as `people.enc` like the
//! drafts, removed at sign-out); no server, bridge or other person learns them, and a mention still
//! carries the person's own name.

use std::path::Path;

use super::*;
use matrix_sdk::ruma::UserId;
use crate::i18n::t;
use gpui_kit::component::input::{Textarea, TextareaState};

/// Signal's limits: 26 characters for each half of a name (one field here) and 240 for a note.
pub(super) const NICK_MAX: usize = 52;
pub(super) const NOTE_MAX: usize = 240;
/// People with a nickname or a note kept at most; the oldest set goes first.
const MAX_PEOPLE: usize = 1000;
const FILE: &str = crate::vault::PEOPLE;

#[derive(Clone, Default, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct About {
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub note: String,
}

/// The nicknames and notes of one account, oldest set first: at most `MAX_PEOPLE` entries of
/// at most 292 characters each, under 1.2 MB, read once and written on each change.
pub(super) struct People {
    dir: PathBuf,
    seal: crate::vault::Seal,
    items: Vec<(OwnedUserId, About)>,
    /// The stored file was read, or there is none: only then may it be written over.
    loaded: bool,
    /// The name each chat had before a nickname took its place, to give it back.
    real: HashMap<OwnedRoomId, String>,
}

impl People {
    pub(super) fn load(dir: &Path, seal: crate::vault::Seal) -> Self {
        let mut people = Self { dir: dir.to_owned(), seal, items: Vec::new(), loaded: false, real: HashMap::new() };
        people.read();
        people
    }

    fn read(&mut self) {
        use crate::vault::{Broken, broken};
        let stored: Vec<(String, About)> = match self.seal.read(&self.dir, FILE) {
            Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|e| {
                log::warn!("{FILE}: {e}");
                Vec::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) if matches!(broken(&e), Some(Broken::Short | Broken::Tag)) => {
                log::warn!("{e}");
                Vec::new()
            }
            // May pass: kept, and nothing is written over it until it is read.
            Err(e) => {
                log::warn!("{e}");
                return;
            }
        };
        for (user, about) in stored {
            if let Ok(user) = OwnedUserId::try_from(user)
                && self.get(&user).is_none()
            {
                self.items.push((user, clip(about)));
            }
        }
        let over = self.items.len().saturating_sub(MAX_PEOPLE);
        self.items.drain(..over);
        self.loaded = true;
    }

    pub(super) fn get(&self, user: &UserId) -> Option<&About> {
        self.items.iter().find(|(id, _)| id == user).map(|(_, about)| about)
    }

    pub(super) fn nick(&self, user: &UserId) -> Option<&str> {
        self.get(user).map(|a| a.nick.as_str()).filter(|n| !n.is_empty())
    }

    /// Keeps what was typed, cut to the limits; both empty forgets the person. True when it changed.
    fn set(&mut self, user: &OwnedUserId, about: About) -> bool {
        let about = clip(about);
        if self.get(user) == Some(&about) || self.get(user).is_none() && about == About::default() {
            return false;
        }
        self.items.retain(|(id, _)| id != user);
        if about != About::default() {
            self.items.push((user.clone(), about));
            if self.items.len() > MAX_PEOPLE {
                self.items.remove(0);
            }
        }
        true
    }

    fn save(&mut self) {
        if !self.loaded {
            self.read();
            if !self.loaded {
                return;
            }
        }
        let written = serde_json::to_vec(&self.items).map_err(std::io::Error::other).and_then(|raw| self.seal.write(&self.dir, FILE, &raw));
        if let Err(e) = written {
            log::warn!("{FILE}: {e}");
        }
    }

    /// Puts the nickname in place of the name of each one-to-one chat with a nicknamed person, and
    /// gives a chat its own name back once the nickname is gone.
    pub(super) fn rename(&mut self, rooms: &mut [RoomSummary]) {
        for r in rooms {
            let nick = r.peer.as_deref().and_then(|peer| self.nick(peer)).map(str::to_owned);
            match nick {
                Some(nick) if r.name != nick => {
                    let real = std::mem::replace(&mut r.name, nick);
                    self.real.insert(r.id.clone(), real);
                }
                Some(_) => {}
                None => {
                    if let Some(real) = self.real.remove(&r.id) {
                        r.name = real;
                    }
                }
            }
        }
    }

    /// A chat's update brings its own name: the next `rename` sets the nickname again.
    pub(super) fn arrived(&mut self, room: &OwnedRoomId) {
        self.real.remove(room);
    }
}

fn clip(about: About) -> About {
    let cut = |s: &str, max: usize| s.trim().chars().take(max).collect::<String>().trim().to_owned();
    About { nick: cut(&about.nick, NICK_MAX).replace(['\n', '\r'], " "), note: cut(&about.note, NOTE_MAX) }
}

/// The page that sets a person's nickname and note.
pub(super) struct Editing {
    pub user: OwnedUserId,
    /// Their own name, shown over the fields.
    pub name: String,
    nick: Entity<InputState>,
    note: Entity<TextareaState>,
    _subs: [Subscription; 2],
}

impl BeeperApp {
    /// The name to show for `user`: the user's nickname for them, else `name`.
    pub(super) fn shown_name(&self, user: &UserId, name: &str) -> String {
        self.nicks.nick(user).unwrap_or(name).to_owned()
    }

    pub(super) fn about(&self, user: &UserId) -> Option<&About> {
        self.nicks.get(user)
    }

    /// The chat's own name while a nickname stands in its place.
    pub(super) fn people_real_name(&self, room: &OwnedRoomId) -> Option<&str> {
        self.nicks.real.get(room).map(String::as_str)
    }

    /// The row that opens the page, for a person's info or sheet.
    pub(super) fn nickname_row(&self, user: OwnedUserId, name: String, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let label = if self.nicks.get(&user).is_some() { t!("Edit nickname and note") } else { t!("Add nickname and note") };
        sheet_row("person-nickname", IconName::UserPen, label, p.text, window, cx).on_click(cx.listener(move |this, _, window, cx| {
            this.chat_sheet = None;
            this.open_nickname(user.clone(), name.clone(), window, cx);
        }))
    }

    fn open_nickname(&mut self, user: OwnedUserId, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let about = self.nicks.get(&user).cloned().unwrap_or_default();
        self.info_face(&user);
        let nick = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Nickname")));
        let note = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder(t!("Note")));
        nick.update(cx, |input, cx| input.set_value(about.nick, window, cx));
        note.update(cx, |input, cx| input.set_value(about.note, window, cx));
        crate::ime::follow(&nick, window, cx);
        crate::ime::follow(&note, window, cx);
        let subs = [
            cx.subscribe_in(&nick, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Text, cx)),
            cx.subscribe_in(&note, window, |_, _, ev: &InputEvent, _, cx| {
                keyboard_follows(ev, Keyboard::Text, cx);
                cx.notify();
            }),
        ];
        self.info.page = Some(group::Page::Nick(Box::new(Editing { user, name, nick, note, _subs: subs })));
        cx.notify();
    }

    fn save_nickname(&mut self, clear: bool, cx: &mut Context<Self>) {
        let Some(group::Page::Nick(e)) = &self.info.page else { return };
        let about = if clear { About::default() } else { About { nick: e.nick.read(cx).value().to_string(), note: e.note.read(cx).value().to_string() } };
        let user = e.user.clone();
        if self.nicks.set(&user, about) {
            self.nicks.save();
            self.nicks.rename(&mut self.rooms);
            self.refilter(cx);
        }
        self.info.page = None;
        self.info_rows();
        cx.notify();
    }

    pub(super) fn render_nickname_page(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(group::Page::Nick(e)) = &self.info.page else { return None };
        let p = Palette::of(cx);
        let label = |text: &'static str| div().text_size(fs(12.)).text_color(p.faint).child(text);
        let left = NOTE_MAX.saturating_sub(e.note.read(cx).value().chars().count());
        let set = self.nicks.get(&e.user).is_some();
        Some(
            v_flex()
                .id("nickname-page")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(GUTTER))
                .pt(px(8.))
                .pb(px(GAP * 2.))
                .gap(px(GAP))
                .child(
                    v_flex()
                        .items_center()
                        .gap(px(GAP))
                        .child(avatar(&e.name, self.faces.get(&e.user), 96., cx))
                        .child(div().text_size(fs(14.)).text_color(p.muted).child(e.name.clone())),
                )
                .child(v_flex().gap(px(4.)).child(label(t!("Nickname"))).child(Input::new(&e.nick)).child(crate::ime::marks(&e.nick)))
                .child(
                    v_flex()
                        .gap(px(4.))
                        .child(label(t!("Note")))
                        .child(Textarea::new(&e.note))
                        .child(crate::ime::marks(&e.note))
                        .child(div().text_size(fs(12.)).text_color(p.faint).child(t!("{n} characters left", n = left))),
                )
                .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(t!("Nicknames and notes stay on this device. Only you see them; the person and the server do not.")))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(Button::new("nickname-save").primary().label(t!("Save")).on_click(cx.listener(|this, _, _, cx| this.save_nickname(false, cx))))
                        .when(set, |el| el.child(Button::new("nickname-delete").label(t!("Delete")).on_click(cx.listener(|this, _, _, cx| this.save_nickname(true, cx))))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use std::{collections::HashMap, path::PathBuf};

    use matrix_sdk::ruma::OwnedUserId;

    use super::{About, MAX_PEOPLE, NICK_MAX, NOTE_MAX, People};
    use crate::backend::RoomSummary;
    use crate::vault::Seal;

    fn people() -> People {
        People { dir: PathBuf::new(), seal: Seal::Plain, items: Vec::new(), loaded: true, real: HashMap::new() }
    }

    fn user(n: usize) -> OwnedUserId {
        OwnedUserId::try_from(format!("@u{n}:x.org")).unwrap()
    }

    fn about(nick: &str, note: &str) -> About {
        About { nick: nick.into(), note: note.into() }
    }

    #[test]
    fn set_clips_forgets_and_caps() {
        let mut p = people();
        assert!(!p.set(&user(1), about("  ", "")), "nothing to keep");
        assert!(p.set(&user(1), about(&"ż".repeat(NICK_MAX + 9), &"n".repeat(NOTE_MAX + 1))));
        let got = p.get(&user(1)).unwrap();
        assert_eq!((got.nick.chars().count(), got.note.chars().count()), (NICK_MAX, NOTE_MAX));
        assert!(p.set(&user(1), about("Mum\nhome", "")));
        assert_eq!(p.nick(&user(1)), Some("Mum home"));
        assert!(p.set(&user(1), About::default()));
        assert!(p.get(&user(1)).is_none());
        for n in 0..MAX_PEOPLE + 3 {
            p.set(&user(n), about("x", ""));
        }
        assert_eq!(p.items.len(), MAX_PEOPLE);
        assert!(p.get(&user(0)).is_none(), "the oldest goes first");
    }

    #[test]
    fn nickname_names_the_chat_and_gives_its_name_back() {
        let mut p = people();
        let mut rooms: Vec<RoomSummary> = vec![serde_json::from_value(serde_json::json!({ "id": "!a:x.org", "name": "Ann Smith", "ts": 0, "peer": "@u1:x.org" })).unwrap()];
        p.set(&user(1), about("Mum", ""));
        p.rename(&mut rooms);
        assert_eq!(rooms[0].name, "Mum");
        p.set(&user(1), about("", "likes tea"));
        p.rename(&mut rooms);
        assert_eq!(rooms[0].name, "Ann Smith");
        p.set(&user(1), about("Mum", ""));
        p.rename(&mut rooms);
        // The chat's own update: its name comes fresh from the server.
        rooms[0].name = "Ann S.".into();
        p.arrived(&rooms[0].id);
        p.rename(&mut rooms);
        p.set(&user(1), About::default());
        p.rename(&mut rooms);
        assert_eq!(rooms[0].name, "Ann S.");
    }

    #[test]
    fn kept_sealed_across_a_restart() {
        let dir = std::env::temp_dir().join(format!("unisend-people-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::vault::SESSION), b"{}").unwrap();
        let sealed = Seal::Sealed(std::sync::Arc::new(zeroize::Zeroizing::new([7; crate::vault::KEY_LEN])));
        let mut p = People::load(&dir, sealed.clone());
        p.set(&user(2), about("Boss", "secret thing"));
        p.save();
        assert!(!std::fs::read(dir.join("people.enc")).unwrap().windows(6).any(|w| w == b"secret"));
        assert_eq!(People::load(&dir, sealed).get(&user(2)), Some(&about("Boss", "secret thing")));
        crate::vault::wipe(&dir);
        assert!(!dir.join("people.enc").exists(), "signing out removes them");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
