//! Desktop keyboard shortcuts: the one table the key handler (`BeeperApp::shortcut`) and the
//! "Keyboard shortcuts" sheet both read, so the sheet cannot list a key the app ignores.

use super::sheet::SheetMotion;
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Act {
    Search,
    NewChat,
    PrevChat,
    NextChat,
    PrevUnread,
    NextUnread,
    FindInChat,
    Settings,
    Emoji,
    Archive,
    MarkUnread,
    Shortcuts,
    Paste,
    EditLast,
    /// Escape goes back through `go_back`, outside `shortcut`; listed so the sheet shows it.
    Back,
}

/// `cmd` is the platform's secondary modifier: Ctrl, and Cmd on macOS.
pub(super) struct Shortcut {
    pub key: &'static str,
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub act: Act,
    pub label: &'static str,
}

const fn s(key: &'static str, cmd: bool, shift: bool, alt: bool, act: Act, label: &'static str) -> Shortcut {
    Shortcut { key, cmd, shift, alt, act, label }
}

pub(super) static SHORTCUTS: &[Shortcut] = &[
    s("k", true, false, false, Act::Search, n_!("Search chats")),
    s("f", true, false, false, Act::Search, n_!("Search chats")),
    s("f", true, true, false, Act::FindInChat, n_!("Search this chat")),
    s("n", true, false, false, Act::NewChat, n_!("New chat")),
    s(",", true, false, false, Act::Settings, n_!("Settings")),
    s("up", false, false, true, Act::PrevChat, n_!("Previous chat")),
    s("down", false, false, true, Act::NextChat, n_!("Next chat")),
    s("up", false, true, true, Act::PrevUnread, n_!("Previous unread chat")),
    s("down", false, true, true, Act::NextUnread, n_!("Next unread chat")),
    s("j", true, true, false, Act::Emoji, n_!("Emoji picker")),
    s("a", true, true, false, Act::Archive, n_!("Archive or unarchive the chat")),
    s("u", true, true, false, Act::MarkUnread, n_!("Mark the chat as unread or read")),
    s("v", true, false, false, Act::Paste, n_!("Paste files")),
    s("up", false, false, false, Act::EditLast, n_!("Edit your last message")),
    s("escape", false, false, false, Act::Back, n_!("Go back")),
    s("/", true, false, false, Act::Shortcuts, n_!("Keyboard shortcuts")),
];

/// The action bound to exactly these modifiers and this key.
pub(super) fn find(k: &Keystroke) -> Option<Act> {
    let m = &k.modifiers;
    // Control and Cmd are one slot: the other of the two must not be held.
    let held = usize::from(m.control) + usize::from(m.platform);
    SHORTCUTS.iter().find(|s| s.key == k.key && m.secondary() == s.cmd && held == usize::from(s.cmd) && m.shift == s.shift && m.alt == s.alt).map(|s| s.act)
}

fn caps(s: &Shortcut) -> Vec<String> {
    let mac = cfg!(target_os = "macos");
    let mut caps = Vec::new();
    if s.cmd {
        caps.push(if mac { "\u{2318}" } else { "Ctrl" }.to_owned());
    }
    if s.alt {
        caps.push(if mac { "\u{2325}" } else { "Alt" }.to_owned());
    }
    if s.shift {
        caps.push(if mac { "\u{21E7}" } else { "Shift" }.to_owned());
    }
    caps.push(match s.key {
        "up" => "\u{2191}".to_owned(),
        "down" => "\u{2193}".to_owned(),
        "escape" => "Esc".to_owned(),
        key => key.to_uppercase(),
    });
    caps
}

/// The sheet's own state: whether it is up, and its motion.
#[derive(Default)]
pub(super) struct Keys {
    pub open: bool,
    pub motion: SheetMotion,
}

impl BeeperApp {
    pub(super) fn toggle_keys(&mut self, cx: &mut Context<Self>) {
        self.keys.open = !self.keys.open;
        self.sheet_at = None;
        cx.notify();
    }

    pub(super) fn render_keys_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let frame = self.keys.motion.sample(self.keys.open, window, cx)?;
        let p = Palette::of(cx);
        let cap = |text: String| {
            div().min_w(px(24.)).h(px(24.)).px(px(6.)).flex().items_center().justify_center().rounded(px(6.)).bg(p.raised).text_color(p.text).text_size(fs(12.)).font_weight(FontWeight::MEDIUM).child(text)
        };
        let rows = SHORTCUTS.iter().map(|s| {
            h_flex()
                .h(px(40.))
                .px(px(GUTTER + 4.))
                .gap(px(GAP))
                .justify_between()
                .text_size(fs(15.))
                .child(div().min_w_0().truncate().child(tr(s.label)))
                .child(h_flex().flex_none().gap(px(4.)).children(caps(s).into_iter().map(cap)))
        });
        let list = v_flex().id("shortcut-list").max_h(px((f32::from(frame::viewport(window).height) - 160.).max(120.))).overflow_y_scroll().children(rows);
        Some(self.sheet(&self.keys.motion, frame, 0, t!("Keyboard shortcuts").to_owned(), vec![list], |this| &mut this.keys.motion, window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::SHORTCUTS;

    #[test]
    fn no_two_entries_share_a_key_combination() {
        let mut seen = std::collections::HashSet::new();
        for s in SHORTCUTS {
            assert!(seen.insert((s.key, s.cmd, s.shift, s.alt)));
        }
    }
}
