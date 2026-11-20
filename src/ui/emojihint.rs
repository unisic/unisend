//! Emoji by name while typing, as Signal offers them: `:` at the start of a word and two letters
//! or more list the emoji whose shortcode or name begins with them over the composer, recent ones
//! first; picking one puts it in place of what was typed. Data is the emoji picker's (`emojis`
//! crate, in the picker's skin tone), so the list offers only what the emoji font draws.
//!
//! Cost: each keystroke while a query is open scans the ~1900 emoji once (one lowercased name
//! each), a few tens of microseconds; nothing is kept but `MAX_HITS` indices.

use gpui_kit::component::input::{IndentInline, MoveDown, MoveUp};

use super::*;
use crate::emoji;

/// Rows the list shows at most.
const MAX_HITS: usize = 5;
/// Letters after the `:` before the list opens, and the most still read as a name.
const MIN_QUERY: usize = 2;
const MAX_QUERY: usize = 30;
const ROW_H: f32 = 44.;

#[derive(Default)]
pub(super) struct Hints {
    /// Byte offset of the `:` being completed.
    at: Option<usize>,
    hits: Vec<&'static emojis::Emoji>,
    pub(super) selected: usize,
}

impl Hints {
    pub(super) fn open(&self) -> bool {
        self.at.is_some() && !self.hits.is_empty()
    }

    pub(super) fn close(&mut self) {
        *self = Self::default();
    }
}

/// Where a `:` starting a word before `cursor` opens a name being typed, and that name lowercased.
fn query_at(text: &str, cursor: usize) -> Option<(usize, String)> {
    let head = text.get(..cursor)?;
    let at = head.rfind(':')?;
    let typed = &head[at + 1..];
    let starts_word = head[..at].chars().next_back().is_none_or(|c| c.is_whitespace() || "([{\"'".contains(c));
    let n = typed.chars().count();
    let name = typed.chars().all(|c| c.is_alphanumeric() || "_+-".contains(c));
    (starts_word && name && (MIN_QUERY..=MAX_QUERY).contains(&n)).then(|| (at, typed.to_lowercase()))
}

/// Emoji whose shortcode starts with `query`, then those with a word of the name starting with
/// it, then those whose shortcode holds it; within each, `recent` (newest first) lead.
fn rank(candidates: impl Iterator<Item = &'static emojis::Emoji>, query: &str, recent: &[&str]) -> Vec<&'static emojis::Emoji> {
    let mut hits: Vec<(u8, usize, usize, usize, &'static emojis::Emoji)> = candidates
        .enumerate()
        .filter_map(|(i, e)| {
            let fit = if e.shortcodes().any(|s| s.starts_with(query)) {
                0
            } else if e.name().to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(query)) {
                1
            } else if e.shortcodes().any(|s| s.contains(query)) {
                2
            } else {
                return None;
            };
            let lately = recent.iter().position(|r| *r == e.as_str()).unwrap_or(usize::MAX);
            // The closest name first: `hear` puts `heart` before `heart_eyes`.
            let short = e.shortcodes().map(str::len).min().unwrap_or(usize::MAX);
            Some((fit, lately, short, i, e))
        })
        .collect();
    hits.sort_unstable_by_key(|&(fit, lately, short, i, _)| (fit, lately, short, i));
    hits.into_iter().take(MAX_HITS).map(|(.., e)| e).collect()
}

impl BeeperApp {
    /// Reads the composer after a change: opens, narrows or closes the list.
    pub(super) fn emoji_hint_typed(&mut self, cx: &mut Context<Self>) {
        let was_open = self.emoji_hint.open();
        let composer = self.composer.read(cx);
        let query = query_at(&composer.value(), composer.cursor()).filter(|_| !self.mention.open());
        match query {
            Some((at, typed)) => {
                let hits = rank(self.emoji_pick.shown(&emoji::drawable), &typed, self.emoji_pick.recents());
                if self.emoji_hint.hits != hits {
                    self.emoji_hint.selected = 0;
                }
                (self.emoji_hint.at, self.emoji_hint.hits) = (Some(at), hits);
            }
            None => self.emoji_hint.close(),
        }
        if was_open || self.emoji_hint.open() {
            cx.notify();
        }
    }

    /// Puts the chosen emoji into the composer in place of the `:` and what followed it.
    pub(super) fn pick_emoji_hint(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(at), Some(e)) = (self.emoji_hint.at, self.emoji_hint.hits.get(row).copied()) else { return };
        self.emoji_hint.close();
        self.composer.update(cx, |c, cx| {
            c.set_selected_range(at..c.cursor().max(at), cx);
            c.replace(e.as_str(), window, cx);
        });
        self.emoji_pick.used(e.as_str(), &self.dir);
        // A click on the row took focus from the composer; Enter should still send.
        let focus = self.composer.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Up, Down and Tab move in and pick from the list while it is open.
    pub(super) fn emoji_hint_keys(&self, el: Div, cx: &mut Context<Self>) -> Div {
        el.capture_action(cx.listener(|this, _: &MoveUp, _, cx| this.emoji_hint_step(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| this.emoji_hint_step(1, cx)))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if this.emoji_hint.open() {
                    cx.stop_propagation();
                    this.pick_emoji_hint(this.emoji_hint.selected, window, cx);
                }
            }))
    }

    fn emoji_hint_step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.emoji_hint.hits.len();
        if self.emoji_hint.open() {
            cx.stop_propagation();
            self.emoji_hint.selected = (self.emoji_hint.selected as isize + by).rem_euclid(n as isize) as usize;
            cx.notify();
        }
    }

    pub(super) fn render_emoji_hints(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.emoji_hint.open() {
            return None;
        }
        let p = Palette::of(cx);
        let selected = self.emoji_hint.selected;
        Some(
            v_flex()
                .mx(px(GUTTER))
                .mt(px(8.))
                .py(px(4.))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .overflow_hidden()
                .children(self.emoji_hint.hits.iter().enumerate().map(|(row, e)| {
                    let code = e.shortcode().map_or_else(|| e.name().to_owned(), |s| format!(":{s}:"));
                    h_flex()
                        .id(("emoji-hint", row))
                        .h(px(ROW_H))
                        .px(px(12.))
                        .gap(px(10.))
                        .cursor_pointer()
                        .when(row == selected && !gpui_kit::is_mobile(), |el| el.bg(p.raised))
                        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                        .active(|el| el.bg(p.raised))
                        .child(div().w(px(28.)).flex_none().text_size(px(24.)).child(emoji::styled(e.as_str(), Vec::new())))
                        .child(div().flex_1().min_w_0().truncate().text_size(fs(16.)).text_color(p.text).child(code))
                        .on_click(cx.listener(move |this, _, window, cx| this.pick_emoji_hint(row, window, cx)))
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{query_at, rank};

    #[test]
    fn a_colon_starting_a_word_and_two_letters_open_a_query() {
        assert_eq!(query_at("hi :jo", 6), Some((3, "jo".into())));
        assert_eq!(query_at(":Thumbs", 7), Some((0, "thumbs".into())));
        assert_eq!(query_at(":+1", 3), Some((0, "+1".into())));
        assert_eq!(query_at("hi :j", 5), None, "one letter");
        assert_eq!(query_at("at 12:30", 8), None, "inside a word");
        assert_eq!(query_at("see http://x", 12), None);
        assert_eq!(query_at("hi :jo y", 8), None, "a space ends it");
        assert_eq!(query_at("hi :jo", 2), None, "the cursor is before it");
    }

    #[test]
    fn shortcode_starts_lead_and_recents_first_among_them() {
        let all = || emojis::iter().filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default));
        let hits = rank(all(), "joy", &[]);
        assert_eq!(hits[0].as_str(), "\u{1F602}");
        assert!(hits.len() <= 5);
        let joy_cat = "\u{1F639}";
        assert_eq!(rank(all(), "joy", &[joy_cat])[0].as_str(), joy_cat, "a recent one leads its tier");
        assert!(rank(all(), "zzqq", &[]).is_empty());
        assert_eq!(rank(all(), "hear", &[])[0].as_str(), "\u{2764}\u{FE0F}", "the shortest name leads");
        assert!(rank(all(), "thumbs", &[]).iter().any(|e| e.as_str() == "\u{1F44D}"), "by a word of the name");
    }
}
