//! Moving around a chat: the text left unsent in each chat, the jump to the newest message, and
//! the scroll to the first unread one.

use super::*;

/// Chats whose draft is kept, and the longest draft kept.
const MAX_DRAFTS: usize = 200;
const MAX_DRAFT_BYTES: usize = 4096;

/// Unsent composer text by chat, least recently written first. At most `MAX_DRAFTS * MAX_DRAFT_BYTES`
/// (800 kB) in memory and in `drafts.json`, which is rewritten when a chat closes or the app leaves
/// the screen, not on every key.
pub(super) struct Drafts {
    path: PathBuf,
    items: Vec<(OwnedRoomId, String)>,
    dirty: bool,
}

impl Drafts {
    pub(super) fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("drafts.json");
        let stored: Vec<(String, String)> = std::fs::read(&path).ok().and_then(|raw| serde_json::from_slice(&raw).ok()).unwrap_or_default();
        let mut drafts = Self { path, items: Vec::new(), dirty: false };
        for (room, text) in stored {
            if let Ok(room) = OwnedRoomId::try_from(room) {
                drafts.set(&room, &text);
            }
        }
        drafts.dirty = false;
        drafts
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
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let tmp = self.path.with_extension("tmp");
        let written = serde_json::to_vec(&self.items).map_err(std::io::Error::other).and_then(|raw| std::fs::write(&tmp, raw)).and_then(|()| std::fs::rename(&tmp, &self.path));
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
                                    .text_size(px(12.))
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

    fn room(n: usize) -> OwnedRoomId {
        OwnedRoomId::try_from(format!("!r{n}:example.org")).unwrap()
    }

    fn drafts() -> Drafts {
        Drafts { path: PathBuf::new(), items: Vec::new(), dirty: false }
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
        let mut d = Drafts::load(&dir);
        d.set(&room(7), "unsent");
        d.save();
        assert_eq!(Drafts::load(&dir).get(&room(7)), Some("unsent"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
