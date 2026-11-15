//! Moving around a chat: the text left unsent in each chat, the jump to the newest message, and
//! the scroll to the first unread one.

use super::*;

/// Chats whose draft is kept, and the longest draft kept.
const MAX_DRAFTS: usize = 200;
const MAX_DRAFT_BYTES: usize = 4096;

/// Unsent composer text by chat, least recently written first. At most `MAX_DRAFTS * MAX_DRAFT_BYTES`
/// (800 kB) in memory and in `drafts.json` (sealed: `drafts.enc`), which is rewritten when a chat
/// closes or the app leaves the screen, not on every key.
pub(super) struct Drafts {
    dir: PathBuf,
    seal: crate::vault::Seal,
    items: Vec<(OwnedRoomId, String)>,
    dirty: bool,
    /// The stored drafts were read, or there are none: only then may the file be written over.
    loaded: bool,
}

impl Drafts {
    pub(super) fn load(dir: &std::path::Path, seal: crate::vault::Seal) -> Self {
        let mut drafts = Self { dir: dir.to_owned(), seal, items: Vec::new(), dirty: false, loaded: false };
        drafts.merge();
        drafts
    }

    /// Adds the stored drafts of chats without one in memory, as older ones. False when the file
    /// could not be read for a reason that may pass, or holds a newer format: it is kept then.
    fn merge(&mut self) -> bool {
        use crate::vault::{Broken, DRAFTS, broken};
        let stored: Vec<(String, String)> = match self.seal.read(&self.dir, DRAFTS) {
            Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|e| {
                log::warn!("drafts.json: {e}");
                Vec::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            // Damaged, or sealed with another key: nothing in it can be read again.
            Err(e) if matches!(broken(&e), Some(Broken::Short | Broken::Tag)) => {
                log::warn!("{e}");
                Vec::new()
            }
            Err(e) => {
                log::warn!("{e}");
                return false;
            }
        };
        let mut older = Self { dir: PathBuf::new(), seal: crate::vault::Seal::Plain, items: Vec::new(), dirty: false, loaded: true };
        for (room, text) in stored {
            if let Ok(room) = OwnedRoomId::try_from(room)
                && self.get(&room).is_none()
            {
                older.set(&room, &text);
            }
        }
        older.items.append(&mut self.items);
        let over = older.items.len().saturating_sub(MAX_DRAFTS);
        older.items.drain(..over);
        self.items = older.items;
        self.loaded = true;
        true
    }

    pub(super) fn get(&self, room: &OwnedRoomId) -> Option<&str> {
        self.items.iter().find(|(id, _)| id == room).map(|(_, text)| text.as_str())
    }

    /// Keeps `text` as the room's draft; nothing but white space removes it.
    pub(super) fn set(&mut self, room: &OwnedRoomId, text: &str) {
        let text = text.trim_end();
        if self.get(room) == Some(text) || self.get(room).is_none() && text.trim().is_empty() {
            return;
        }
        self.items.retain(|(id, _)| id != room);
        self.dirty = true;
        if text.trim().is_empty() {
            return;
        }
        let mut end = text.len().min(MAX_DRAFT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.items.push((room.clone(), text[..end].to_owned()));
        if self.items.len() > MAX_DRAFTS {
            self.items.remove(0);
        }
    }

    pub(super) fn save(&mut self) {
        // Drafts that could not be read yet are merged in first, never written over; until then
        // the change stays unsaved and the next save tries again.
        if !self.dirty || !self.loaded && !self.merge() {
            return;
        }
        self.dirty = false;
        let written = serde_json::to_vec(&self.items).map_err(std::io::Error::other).and_then(|raw| self.seal.write(&self.dir, crate::vault::DRAFTS, &raw));
        if let Err(e) = written {
            log::warn!("drafts.json: {e}");
        }
    }
}

/// Where the user is in the open chat.
#[derive(Default)]
pub(super) struct Place {
    /// Scrolled up by more than a screen.
    pub away: bool,
    /// Messages from others that arrived meanwhile.
    pub missed: usize,
    /// The chat was just opened: scroll to its first unread message once it is in the list.
    pub seek_unread: bool,
    /// Text the composer is to be given by the next frame: a chat's draft, or nothing.
    pub restore: Option<String>,
    /// The text last given to the composer, so its change is not taken for the user typing.
    pub restored: Option<String>,
}

impl BeeperApp {
    /// A chat opened or closed: the composer shows its draft, or is emptied.
    pub(super) fn show_draft(&mut self, room: Option<&OwnedRoomId>) {
        self.place = Place { restore: Some(room.and_then(|id| self.drafts.get(id)).unwrap_or_default().to_owned()), seek_unread: room.is_some(), ..Place::default() };
    }

    /// Gives the composer what `show_draft` chose. Needs a window, so it waits for the frame.
    pub(super) fn apply_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.place.restore.take()
            && self.composer.read(cx).value().as_ref() != text
        {
            self.place.restored = Some(text.clone());
            self.composer.update(cx, |c, cx| c.set_value(text, window, cx));
        }
    }

    /// The composer changed. False when it was only `apply_draft`.
    pub(super) fn composer_changed(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.composer.read(cx).value().to_string();
        if self.place.restored.take().is_some_and(|r| r == text) {
            return false;
        }
        if let (Some(room), false) = (&self.open, matches!(self.compose, Compose::Edit(_))) {
            self.drafts.set(room, &text);
            cx.notify();
        }
        true
    }

    /// The list scrolled: the button shows once the newest message is over a screen away.
    pub(super) fn scrolled(&mut self, cx: &mut Context<Self>) {
        let list = &self.timeline;
        let view = list.viewport_bounds().size.height;
        let far = view > px(0.) && list.max_offset_for_scrollbar().y + list.scroll_px_offset_for_scrollbar().y > view;
        if far != self.place.away {
            self.place.away = far;
            if !far {
                self.place.missed = 0;
            }
            cx.notify();
        }
    }

    pub(super) fn jump_to_newest(&mut self, cx: &mut Context<Self>) {
        self.timeline.scroll_to_end();
        (self.place.away, self.place.missed) = (false, 0);
        cx.notify();
    }

    /// Once the rows of a freshly opened chat are in, brings its first unread one into view.
    pub(super) fn seek_unread(&mut self) {
        if !self.place.seek_unread {
            return;
        }
        if let Some(ix) = self.msgs.iter().position(|m| m.kind == Kind::Unread) {
            self.place.seek_unread = false;
            self.timeline.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
        }
    }

    /// Counts messages from others that landed at the end while the user reads further up.
    pub(super) fn count_missed(&mut self, from: usize, len: usize) {
        if self.place.away {
            let end = (from + len).min(self.msgs.len());
            self.place.missed += self.msgs.get(from..end).map_or(0, |rows| rows.iter().filter(|m| m.kind == Kind::Message && !m.own).count());
        }
    }

    pub(super) fn jump_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.place.away {
            return None;
        }
        let p = Palette::of(cx);
        let missed = self.place.missed;
        Some(
            div()
                .absolute()
                .bottom(px(12.))
                .right(px(GUTTER))
                .child(
                    div()
                        .id("jump-to-newest")
                        .relative()
                        .size(px(TAP))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(p.rule)
                        .bg(p.raised)
                        .text_color(p.text)
                        .cursor_pointer()
                        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.lifted)))
                        .active(|el| el.bg(p.lifted))
                        .on_click(cx.listener(|this, _, _, cx| this.jump_to_newest(cx)))
                        .child(Icon::new(IconName::ArrowDown).size_5())
                        .when(missed > 0, |el| {
                            el.child(
                                div()
                                    .absolute()
                                    .top(px(-8.))
                                    .right(px(-4.))
                                    .min_w(px(20.))
                                    .h(px(20.))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(p.text)
                                    .text_color(p.ground)
                                    .text_size(fs(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .font_features(tabular())
                                    .child(if missed > 99 { "99+".to_owned() } else { missed.to_string() }),
                            )
                        }),
                )
                .with_animation("jump-to-newest", Animation::new(motion::ENTER).with_easing(motion::ease_out), |el, t| el.opacity(t).bottom(px(4. + 8. * t)))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use matrix_sdk::ruma::OwnedRoomId;

    use super::{Drafts, MAX_DRAFT_BYTES, MAX_DRAFTS};
    use crate::vault::Seal;

    fn room(n: usize) -> OwnedRoomId {
        OwnedRoomId::try_from(format!("!r{n}:example.org")).unwrap()
    }

    fn drafts() -> Drafts {
        Drafts { dir: PathBuf::new(), seal: Seal::Plain, items: Vec::new(), dirty: false, loaded: true }
    }

    #[test]
    fn keeps_trims_and_removes() {
        let mut d = drafts();
        d.set(&room(1), "hello  ");
        assert_eq!(d.get(&room(1)), Some("hello"));
        d.set(&room(1), "  ");
        assert_eq!(d.get(&room(1)), None);
    }

    #[test]
    fn bounded_in_count_and_size() {
        let mut d = drafts();
        for n in 0..MAX_DRAFTS + 5 {
            d.set(&room(n), "x");
        }
        assert_eq!(d.items.len(), MAX_DRAFTS);
        assert_eq!(d.get(&room(0)), None);
        d.set(&room(1000), &"é".repeat(MAX_DRAFT_BYTES));
        assert!(d.get(&room(1000)).unwrap().len() <= MAX_DRAFT_BYTES);
    }

    #[test]
    fn survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("murmur-drafts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::vault::SESSION), b"{}").unwrap();
        let mut d = Drafts::load(&dir, Seal::Plain);
        d.set(&room(7), "unsent");
        d.save();
        assert_eq!(Drafts::load(&dir, Seal::Plain).get(&room(7)), Some("unsent"));
        let sealed = Seal::Sealed(std::sync::Arc::new(zeroize::Zeroizing::new([9; crate::vault::KEY_LEN])));
        let mut d = Drafts::load(&dir, sealed.clone());
        d.set(&room(8), "secret plan");
        d.save();
        assert!(!std::fs::read(dir.join("drafts.enc")).unwrap().windows(6).any(|w| w == b"secret"));
        assert_eq!(Drafts::load(&dir, sealed).get(&room(8)), Some("secret plan"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unread_drafts_are_never_written_over() {
        let dir = std::env::temp_dir().join(format!("murmur-drafts-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::vault::SESSION), b"{}").unwrap();
        let sealed = Seal::Sealed(std::sync::Arc::new(zeroize::Zeroizing::new([9; crate::vault::KEY_LEN])));
        let mut d = Drafts::load(&dir, sealed.clone());
        d.set(&room(1), "kept");
        d.save();
        let stored = std::fs::read(dir.join("drafts.enc")).unwrap();
        // A read error that may pass: the change waits, the file stays as it was.
        std::fs::rename(dir.join("drafts.enc"), dir.join("aside")).unwrap();
        std::fs::create_dir(dir.join("drafts.enc")).unwrap();
        let mut d = Drafts::load(&dir, sealed.clone());
        d.set(&room(2), "new");
        d.save();
        assert!(dir.join("drafts.enc").is_dir() && d.dirty);
        // Readable again: the next save keeps both.
        std::fs::remove_dir(dir.join("drafts.enc")).unwrap();
        std::fs::rename(dir.join("aside"), dir.join("drafts.enc")).unwrap();
        assert_eq!(std::fs::read(dir.join("drafts.enc")).unwrap(), stored);
        d.save();
        let again = Drafts::load(&dir, sealed.clone());
        assert_eq!((again.get(&room(1)), again.get(&room(2))), (Some("kept"), Some("new")));
        // Signed out meanwhile: a late save does not bring the file back.
        crate::vault::wipe(&dir);
        d.set(&room(3), "late");
        d.save();
        assert!(!dir.join("drafts.enc").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
