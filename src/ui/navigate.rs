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
    /// The first message the user had not seen when the chat opened: the unread line stays above
    /// it while the chat is open, though reading moves the SDK's marker to the end.
    pub unread: Option<String>,
    /// How many messages from others were unread then: the line counts them as Signal does.
    pub unread_count: usize,
    /// Text the composer is to be given by the next frame: a chat's draft, or nothing.
    pub restore: Option<String>,
    /// The text last given to the composer, so its change is not taken for the user typing.
    pub restored: Option<String>,
    /// When the user last scrolled the chat: the day being read floats over it until `PILL_LINGER` after.
    pub scrolled_at: Option<Instant>,
    /// A timer to hide the day is running; one at a time.
    pub pill_timer: bool,
}

/// How long the floating day stays after scrolling stops.
const PILL_LINGER: Duration = Duration::from_secs(1);

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
        let first = self.place.scrolled_at.is_none_or(|at| at.elapsed() >= PILL_LINGER);
        self.place.scrolled_at = Some(Instant::now());
        if first {
            cx.notify();
        }
        if !std::mem::replace(&mut self.place.pill_timer, true) {
            cx.spawn(async move |this, cx| {
                let mut wait = PILL_LINGER;
                loop {
                    cx.background_executor().timer(wait).await;
                    let Ok(left) = this.update(cx, |this, cx| {
                        let left = this.place.scrolled_at.map_or(Duration::ZERO, |at| PILL_LINGER.saturating_sub(at.elapsed()));
                        if left.is_zero() {
                            this.place.pill_timer = false;
                            cx.notify();
                        }
                        left
                    }) else {
                        return;
                    };
                    if left.is_zero() {
                        return;
                    }
                    wait = left;
                }
            })
            .detach();
        }
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

    /// Once the rows of a freshly opened chat are in, marks its first unread one and brings it into view.
    pub(super) fn seek_unread(&mut self) {
        if !self.place.seek_unread {
            return;
        }
        if let Some(ix) = first_unseen(&self.msgs) {
            self.place.seek_unread = false;
            self.place.unread = Some(self.msgs[ix].id.clone());
            self.place.unread_count = self.msgs[ix..].iter().filter(|m| m.kind == Kind::Message && !m.own && !self.is_hidden(&m.id)).count().max(1);
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

    /// The day of the topmost message on screen, floating at the top while the user scrolls.
    pub(super) fn date_pill(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let present = self.place.scrolled_at.is_some_and(|at| at.elapsed() < PILL_LINGER);
        let room = self.open.as_ref().map(ToString::to_string).unwrap_or_default();
        let enter = Transition::new(motion::EXIT).easing(motion::EASE_OUT);
        let shown = Presence::new(("date-pill", room), present).transition(if present { enter } else { motion::exit() }).sample(window, cx);
        if !shown.should_render() {
            return None;
        }
        let top = self.timeline.logical_scroll_top().item_ix;
        let label = day_label(self.msgs.get(top)?.ts);
        let p = Palette::of(cx);
        Some(
            div()
                .absolute()
                .top(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .opacity(shown.progress)
                .child(div().h(px(24.)).px(px(10.)).flex().items_center().rounded_full().bg(p.raised).text_size(fs(12.)).text_color(p.muted).child(label))
                .into_any_element(),
        )
    }

    pub(super) fn jump_button(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let room = self.open.as_ref().map(ToString::to_string).unwrap_or_default();
        let away = self.place.away;
        let shown = Presence::new(("jump-to-newest", room), away).transition(if away { motion::enter() } else { motion::exit() }).sample(window, cx);
        if !shown.should_render() {
            return None;
        }
        let v = shown.progress;
        let p = Palette::of(cx);
        let missed = self.place.missed;
        Some(
            div()
                .absolute()
                .bottom(px(4. + 8. * v))
                .right(px(GUTTER))
                .opacity(v)
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
                        .when(away, |el| el.on_click(cx.listener(|this, _, _, cx| this.jump_to_newest(cx))))
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
                .into_any_element(),
        )
    }
}

/// The first message from someone else after the SDK's read marker. The marker stops before any
/// event of another's, also one drawn as nothing; with no such message after it, nothing is new.
pub(super) fn first_unseen(msgs: &[Msg]) -> Option<usize> {
    let mark = msgs.iter().position(|m| m.kind == Kind::Unread)?;
    msgs[mark..].iter().position(|m| m.kind == Kind::Message && !m.own).map(|ix| mark + ix)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use matrix_sdk::ruma::{OwnedRoomId, user_id};

    use super::{Drafts, MAX_DRAFT_BYTES, MAX_DRAFTS, first_unseen};
    use crate::backend::{Delivery, Kind, Msg};
    use crate::vault::Seal;

    #[test]
    fn new_messages_need_one_from_someone_else_after_the_marker() {
        let row = |kind: Kind, own: bool| Msg { own, ..Msg::bare(String::new(), kind, user_id!("@a:x").to_owned()) };
        let status = || Kind::Status { target: String::new(), delivery: Delivery::Sent };
        let read = [row(Kind::Message, false), row(Kind::Message, true)];
        assert_eq!(first_unseen(&read), None, "no marker");
        let bottom = [row(Kind::Message, false), row(Kind::Unread, false), row(Kind::Message, true), row(status(), false), row(Kind::Hidden, false)];
        assert_eq!(first_unseen(&bottom), None, "only the user's own and rows drawn as nothing follow");
        assert_eq!(first_unseen(&[row(Kind::Message, false), row(Kind::Unread, false)]), None, "marker last");
        let unread = [row(Kind::Message, true), row(Kind::Unread, false), row(Kind::Hidden, false), row(Kind::Day, false), row(Kind::Message, false)];
        assert_eq!(first_unseen(&unread), Some(4));
    }

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
        let dir = std::env::temp_dir().join(format!("unisend-drafts-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("unisend-drafts-keep-{}", std::process::id()));
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
