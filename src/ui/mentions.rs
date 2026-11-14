//! The mention list: typing `@` in the composer lists the chat's members, narrowed as more is
//! typed; picking one puts them in the text as one unit, which leaves as a pill (see
//! `backend/mentions.rs`).
//!
//! The list follows every keystroke, so it appears and changes without animation.

use gpui_kit::component::input::{InlineToken, InlineTokenContext, MoveDown, MoveUp};

use super::*;

/// Rows the list shows at most.
const MAX_HITS: usize = 5;
/// Longest query, in characters, still read as the start of a name.
const MAX_QUERY: usize = 40;
/// People picked while the composer could not hold them as a unit (an IME composition was open).
const MAX_PICKED: usize = 32;
const ROW_H: f32 = 44.;
const FACE: f32 = 28.;

#[derive(Default)]
pub(super) struct Mentions {
    /// The chat `members` belong to, or were asked for.
    room: Option<OwnedRoomId>,
    /// (user, name), sorted by name; at most `backend::MAX_MEMBERS`, about 100 B each.
    members: Vec<(OwnedUserId, String)>,
    /// Byte offset of the `@` being completed, and what follows it up to the cursor.
    query: Option<(usize, String)>,
    /// Indices into `members`, best first.
    hits: Vec<usize>,
    pub(super) selected: usize,
    /// (name, user) picked as plain text; `send_message` adds the composer's tokens.
    picked: Vec<(String, OwnedUserId)>,
}

impl Mentions {
    pub(super) fn open(&self) -> bool {
        self.query.is_some() && !self.hits.is_empty()
    }

    pub(super) fn close(&mut self) {
        (self.query, self.selected) = (None, 0);
        self.hits.clear();
    }
}

/// Where an `@` before `cursor` starts a mention being typed, and the text after it.
fn query_at(text: &str, cursor: usize) -> Option<(usize, String)> {
    let head = text.get(..cursor)?;
    let at = head.rfind('@')?;
    let typed = &head[at + 1..];
    let starts_word = head[..at].chars().next_back().is_none_or(|c| c.is_whitespace() || "([{\"'".contains(c));
    let fits = typed.chars().count() <= MAX_QUERY && !typed.contains('\n') && !typed.starts_with(char::is_whitespace);
    (starts_word && fits).then(|| (at, typed.to_owned()))
}

/// Members whose name, or a word of it, or user ID starts with `query`; those who wrote lately
/// (`recent`, newest first) lead.
fn rank(members: &[(OwnedUserId, String)], query: &str, recent: &[&OwnedUserId]) -> Vec<usize> {
    let q = query.to_lowercase();
    let mut hits: Vec<(usize, usize, usize)> = members
        .iter()
        .enumerate()
        .filter_map(|(i, (user, name))| {
            let name = name.to_lowercase();
            let fit = if name.starts_with(&q) {
                0
            } else if name.split_whitespace().any(|w| w.starts_with(&q)) {
                1
            } else if user.localpart().to_lowercase().starts_with(&q) {
                2
            } else {
                return None;
            };
            let lately = recent.iter().position(|r| *r == user).unwrap_or(usize::MAX);
            Some((lately, fit, i))
        })
        .collect();
    hits.sort_unstable();
    hits.into_iter().take(MAX_HITS).map(|(_, _, i)| i).collect()
}

impl BeeperApp {
    /// Reads the composer after a change: opens, narrows or closes the list.
    pub(super) fn mention_typed(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        if self.mention.room.as_ref() != Some(&room) {
            self.mention = Mentions { room: Some(room.clone()), ..Mentions::default() };
        }
        let composer = self.composer.read(cx);
        let text = composer.value();
        let in_token = |at: usize| composer.tokens().iter().any(|t| t.range().contains(&at));
        let query = query_at(&text, composer.cursor()).filter(|(at, _)| !in_token(*at));
        let was_open = self.mention.open();
        let Some((at, typed)) = query else {
            self.mention.close();
            if was_open {
                cx.notify();
            }
            return;
        };
        if self.mention.query.is_none() && self.mention.members.is_empty() {
            // Asked again on each new `@` until the list arrives; the backend reads its store.
            let _ = self.cmd.try_send(Cmd::Members(room));
        }
        self.mention.query = Some((at, typed));
        self.mention_rank();
        if was_open || self.mention.open() {
            cx.notify();
        }
    }

    fn mention_rank(&mut self) {
        let Some((_, typed)) = &self.mention.query else { return };
        let mut recent: Vec<&OwnedUserId> = Vec::new();
        for m in self.msgs.iter().rev().filter(|m| !m.own) {
            if !recent.contains(&&m.from) {
                recent.push(&m.from);
                if recent.len() == 20 {
                    break;
                }
            }
        }
        let hits = rank(&self.mention.members, typed, &recent);
        // A space may be part of a name; once nothing matches, the `@` was not a mention.
        if hits.is_empty() && typed.contains(char::is_whitespace) {
            self.mention.close();
            return;
        }
        if hits != self.mention.hits {
            self.mention.selected = 0;
        }
        self.mention.hits = hits;
    }

    pub(super) fn members_arrived(&mut self, room: OwnedRoomId, list: Vec<(OwnedUserId, String)>, cx: &mut Context<Self>) {
        if self.mention.room.as_ref() == Some(&room) {
            self.mention.members = list;
            self.mention_rank();
            cx.notify();
        }
    }

    /// Puts the chosen member into the composer in place of the `@` and what followed it.
    pub(super) fn pick_mention(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((at, _)) = self.mention.query.clone() else { return };
        let Some((user, name)) = self.mention.hits.get(row).and_then(|&i| self.mention.members.get(i)).cloned() else { return };
        self.mention.close();
        let text = format!("@{name}");
        let held = self.composer.update(cx, |c, cx| {
            let range = at..c.cursor().max(at);
            let held = c.replace_range_with_token(range.clone(), InlineToken::new(user.to_string(), text.clone()), window, cx);
            match held {
                Ok(()) => c.insert(" ", window, cx),
                Err(_) => {
                    c.set_selected_range(range, cx);
                    c.replace(format!("{text} "), window, cx);
                }
            }
            held.is_ok()
        });
        if !held && self.mention.picked.len() < MAX_PICKED {
            self.mention.picked.push((name, user));
        }
        cx.notify();
    }

    /// The people picked for the message being sent, as (name, user).
    pub(super) fn picked_mentions(&mut self, cx: &App) -> Vec<(String, OwnedUserId)> {
        let mut picked = std::mem::take(&mut self.mention.picked);
        for span in self.composer.read(cx).tokens() {
            let token = span.token();
            if let (Some(name), Ok(user)) = (token.text().strip_prefix('@'), OwnedUserId::try_from(token.id().as_ref())) {
                picked.push((name.to_owned(), user));
            }
        }
        picked
    }

    /// Up, Down, Enter and Tab move in and pick from the list while it is open.
    pub(super) fn mention_keys(&self, el: Div, cx: &mut Context<Self>) -> Div {
        el.capture_action(cx.listener(|this, _: &MoveUp, _, cx| this.mention_step(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| this.mention_step(1, cx)))
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::IndentInline, window, cx| {
                if this.mention.open() {
                    cx.stop_propagation();
                    this.pick_mention(this.mention.selected, window, cx);
                }
            }))
    }

    fn mention_step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.mention.hits.len();
        if self.mention.open() {
            cx.stop_propagation();
            self.mention.selected = (self.mention.selected as isize + by).rem_euclid(n as isize) as usize;
            cx.notify();
        }
    }

    pub(super) fn render_mentions(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.mention.open() {
            return None;
        }
        let p = Palette::of(cx);
        let room = self.open.clone()?;
        let rows: Vec<(usize, OwnedUserId, String)> =
            self.mention.hits.iter().enumerate().filter_map(|(row, &i)| self.mention.members.get(i).map(|(u, n)| (row, u.clone(), n.clone()))).collect();
        for (_, user, _) in &rows {
            if !self.faces.contains_key(user) && self.faces.len() < MAX_FACES && self.cmd.try_send(Cmd::Face(room.clone(), user.clone())).is_ok() {
                self.faces.insert(user.clone(), None);
            }
        }
        let selected = self.mention.selected;
        Some(
            v_flex()
                .mx(px(GUTTER))
                .mt(px(8.))
                .py(px(4.))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .overflow_hidden()
                .children(rows.into_iter().map(|(row, user, name)| {
                    h_flex()
                        .id(SharedString::from(format!("mention-{user}")))
                        .h(px(ROW_H))
                        .px(px(12.))
                        .gap(px(10.))
                        .cursor_pointer()
                        .when(row == selected && !gpui_kit::is_mobile(), |el| el.bg(p.raised))
                        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                        .active(|el| el.bg(p.raised))
                        .child(avatar(&name, self.faces.get(&user), FACE, cx))
                        .child(div().flex_1().min_w_0().truncate().text_size(px(16.)).text_color(p.text).child(name))
                        .on_click(cx.listener(move |this, _, window, cx| this.pick_mention(row, window, cx)))
                })),
        )
    }
}

/// A picked person inside the composer: their name in semibold on a faint fill, one unit.
pub(super) fn token(ctx: &InlineTokenContext, cx: &App) -> Div {
    let p = Palette::of(cx);
    div()
        .flex()
        .items_center()
        .h(ctx.line_height())
        .max_w(ctx.available_width())
        .px(px(4.))
        .rounded(px(6.))
        .bg(if ctx.is_selected() { p.raised } else { p.text.opacity(0.08) })
        .text_color(p.text)
        .font_weight(FontWeight::SEMIBOLD)
        .truncate()
        .child(ctx.token().label().clone())
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::owned_user_id;

    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{query_at, rank};

    #[test]
    fn an_at_starting_a_word_opens_a_query() {
        assert_eq!(query_at("hi @an", 6), Some((3, "an".into())));
        assert_eq!(query_at("@", 1), Some((0, String::new())));
        assert_eq!(query_at("hi @Ann Le", 10), Some((3, "Ann Le".into())));
        assert_eq!(query_at("mail a@b.c", 10), None, "inside a word");
        assert_eq!(query_at("hi @ ann", 8), None);
        assert_eq!(query_at("@an\nx", 5), None);
        assert_eq!(query_at("hi @an", 2), None, "the cursor is before it");
    }

    #[test]
    fn ranking_prefers_name_starts_and_recent_writers() {
        let members = vec![
            (owned_user_id!("@zed:x"), "Anna Zed".to_owned()),
            (owned_user_id!("@ann:x"), "Bo Annis".to_owned()),
            (owned_user_id!("@cy:x"), "Cy".to_owned()),
            (owned_user_id!("@anders:x"), "Dee".to_owned()),
        ];
        assert_eq!(rank(&members, "an", &[]), [0, 1, 3]);
        assert_eq!(rank(&members, "AN", &[&members[3].0]), [3, 0, 1]);
        assert_eq!(rank(&members, "", &[]).len(), 4);
        assert!(rank(&members, "q", &[]).is_empty());
    }
}
