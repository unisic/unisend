//! Keyboard reach for the two long lists, the chats and the messages, as listboxes: Tab lands on
//! the list as one stop, the arrows (and Home, End, Page Up, Page Down) move a cursor through its
//! rows, and Enter acts on the row under it. The cursor's row is drawn with a ring and announced
//! to a screen reader as the focused one (`aria_active_descendant`), so the list keeps the focus
//! and a long list costs one Tab, not one per row.

use super::*;

/// How far Page Up and Page Down move the cursor.
const PAGE: usize = 8;

pub(super) struct KeyNav {
    pub chats: FocusHandle,
    pub msgs: FocusHandle,
    /// The first act of a message's menu.
    pub menu: FocusHandle,
    /// The cursor in the chat list, by its place in `shown`.
    pub chat: Option<usize>,
    /// The cursor in the messages, by its place in `msgs`.
    pub msg: Option<usize>,
}

impl KeyNav {
    pub(super) fn new(cx: &mut App) -> Self {
        Self { chats: cx.focus_handle().tab_stop(true), msgs: cx.focus_handle().tab_stop(true), menu: cx.focus_handle().tab_stop(true), chat: None, msg: None }
    }
}

/// Where a key moves a cursor at `at` in `n` rows (`from` when there is none yet); None for a key
/// that is not a move.
fn step(key: &str, at: Option<usize>, from: usize, n: usize) -> Option<usize> {
    let last = n.checked_sub(1)?;
    let at = at.unwrap_or(from).min(last);
    Some(match key {
        "down" => (at + 1).min(last),
        "up" => at.saturating_sub(1),
        "pagedown" => (at + PAGE).min(last),
        "pageup" => at.saturating_sub(PAGE),
        "home" => 0,
        "end" => last,
        _ => return None,
    })
}

/// A plain key: no modifier the app's shortcuts use.
fn plain(k: &Keystroke) -> bool {
    let m = &k.modifiers;
    !(m.control || m.alt || m.platform || m.shift)
}

/// Puts an element in the Tab order with a ring while the keyboard is on it; Enter and Space
/// press it. A pointer's press leaves the focus where it was, so a click keeps the composer's caret.
pub(super) fn reachable(el: Stateful<Div>, cx: &App) -> Stateful<Div> {
    let accent = Palette::of(cx).accent;
    el.tab_index(0)
        .focus_visible(move |s| s.border_2().border_color(accent))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
}

/// The ring that marks the cursor's row, drawn over it so the row keeps its size.
pub(super) fn ring(cx: &App) -> Div {
    div().absolute().inset_0().border_2().border_color(Palette::of(cx).accent).rounded(px(8.))
}

impl BeeperApp {
    /// A key on the focused chat list; true when it was the list's.
    pub(super) fn chat_list_key(&mut self, k: &Keystroke, cx: &mut Context<Self>) -> bool {
        if !plain(k) {
            return false;
        }
        let n = self.shown.len();
        let open = self.open.as_ref().and_then(|open| self.shown.iter().position(|&ix| &self.rooms[ix].id == open));
        if matches!(k.key.as_str(), "enter" | "space") {
            let Some(at) = self.keynav.chat.or(open).filter(|&at| at < n) else { return false };
            let room = &self.rooms[self.shown[at]];
            match room.forum.clone().filter(|f| self.forum_rows.contains_key(f)) {
                Some(forum) => self.open_forum(forum, cx),
                None => self.open_room(room.id.clone(), cx),
            }
            return true;
        }
        let Some(to) = step(&k.key, self.keynav.chat.or(open), 0, n) else { return false };
        self.keynav.chat = Some(to);
        self.rooms_scroll.scroll_to_item(to, ScrollStrategy::Center);
        cx.notify();
        true
    }

    /// A key on the focused messages; true when it was theirs. Enter opens the message's menu.
    pub(super) fn msg_list_key(&mut self, k: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !plain(k) {
            return false;
        }
        let n = self.msgs.len();
        if matches!(k.key.as_str(), "enter" | "space") {
            let Some(at) = self.keynav.msg.filter(|&at| self.msgs.get(at).is_some_and(|m| m.kind == Kind::Message)) else { return false };
            let center = self.timeline.bounds_for_item(at).map_or_else(Point::default, |b| b.center());
            self.open_menu(self.msgs[at].id.clone(), MenuKind::List, center, None, false, cx);
            window.focus(&self.keynav.menu, cx);
            return true;
        }
        let Some(to) = step(&k.key, self.keynav.msg, n.saturating_sub(1), n) else { return false };
        self.keynav.msg = Some(to);
        self.timeline.scroll_to_reveal_item(to);
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn the_cursor_moves_within_the_rows_and_starts_where_it_is_told() {
        assert_eq!(step("down", None, 0, 3), Some(1));
        assert_eq!(step("up", None, 2, 3), Some(1));
        assert_eq!(step("down", Some(2), 0, 3), Some(2));
        assert_eq!(step("up", Some(0), 0, 3), Some(0));
        assert_eq!(step("end", Some(0), 0, 3), Some(2));
        assert_eq!(step("pagedown", Some(0), 0, 20), Some(8));
        assert_eq!(step("down", Some(9), 0, 3), Some(2), "a stale cursor comes back inside");
        assert_eq!(step("down", None, 0, 0), None);
        assert_eq!(step("x", Some(0), 0, 3), None);
    }
}
