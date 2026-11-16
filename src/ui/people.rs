//! Who a message reached and who answered it, as Signal shows them: the message's details (when it
//! was sent, who sent it, who has read it, who it went to) from its menu, and who gave each of its
//! reactions from a tap on them.

use super::sheet::{self, SheetMotion};
use super::*;

/// People a list draws at most; a line under them counts the rest.
const MAX_ROWS: usize = 50;
const PERSON_H: f32 = 56.;
const HEAD_H: f32 = 36.;
const FACT_H: f32 = 44.;
const TABS_H: f32 = 48.;
const FACE: f32 = 36.;
/// The message at the top of its details: at most this many lines, of about this many characters.
const PREVIEW_LINES: usize = 3;
const PREVIEW_CHARS: usize = 30;
/// A list a mouse opened stands at the pointer, at most this tall.
const POPOVER_MAX_H: f32 = 440.;

#[derive(Clone, PartialEq)]
pub(super) enum Shown {
    /// A message's details, by its id.
    Details(String),
    /// Who gave a message's reactions; with a key, only those who gave that one.
    Reactions(String, Option<String>),
}

#[derive(Default)]
pub(super) struct People {
    pub(super) open: Option<Shown>,
    /// What stays drawn while the sheet falls.
    last: Option<Shown>,
    sheet: SheetMotion,
    /// Where a mouse opened it on a desktop: the list stands there, as a menu does.
    at: Option<Point<Pixels>>,
}

/// A list's title, its tabs (the reaction each shows, its label) and its rows.
type Listing = (&'static str, Vec<(Option<String>, String)>, Vec<Line>);

enum Line {
    /// The message itself, as a bubble of `rows` lines.
    Preview { text: String, own: bool, rows: usize },
    Head(&'static str),
    Fact(&'static str, String),
    /// Someone (`None` for the user), with a time or an emoji beside the name. `remove` is the
    /// user's own reaction, which a tap on the row takes back.
    Person { user: Option<OwnedUserId>, aside: String, emoji: bool, remove: Option<String> },
    More(usize),
}

impl Line {
    fn h(&self) -> f32 {
        match self {
            Line::Head(_) | Line::More(_) => HEAD_H,
            Line::Fact(..) => FACT_H,
            Line::Preview { rows, .. } => 2. * 9. + 22. * *rows as f32 + 16.,
            Line::Person { .. } => PERSON_H,
        }
    }
}

/// `users` as rows of a list, the first `MAX_ROWS` of them and a count of the rest.
fn listed(users: impl ExactSizeIterator<Item = (OwnedUserId, String)>) -> Vec<Line> {
    let n = users.len();
    let mut out: Vec<Line> = users.take(MAX_ROWS).map(|(user, aside)| Line::Person { user: Some(user), aside, emoji: false, remove: None }).collect();
    out.extend((n > MAX_ROWS).then(|| Line::More(n - MAX_ROWS)));
    out
}

impl BeeperApp {
    pub(super) fn open_people(&mut self, shown: Shown, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        self.want_members();
        self.audience.at = at.filter(|_| !gpui_kit::is_mobile());
        self.audience.open = Some(shown);
        cx.notify();
    }

    pub(super) fn close_people(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.audience.open.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// Someone's name in the open chat: as a member, as the sender of a loaded message, or the
    /// local part of their address.
    fn person_name(&self, user: &OwnedUserId) -> String {
        let members = self.members();
        let member = members.iter().find(|(id, _)| id == user).map(|(_, name)| name.as_str());
        let sender = || self.msgs.iter().rev().find(|m| m.from == *user && !m.sender.is_empty()).map(|m| m.sender.as_str());
        member.or_else(sender).map_or_else(|| user.localpart().to_owned(), str::to_owned)
    }

    /// The title, the reactions' tabs (key, label) and the rows of what `shown` lists; `None`
    /// once its message is gone, or the last of its reactions.
    fn people_lines(&self, shown: &Shown) -> Option<Listing> {
        match shown {
            Shown::Details(id) => {
                let ix = self.msgs.iter().position(|m| m.id == *id && !m.deleted)?;
                let m = &self.msgs[ix];
                let mut text = m.summary();
                // Cut by characters, not by the layout's clamp, which leaves the bubble as narrow as its ellipsis.
                if let Some((cut, _)) = text.char_indices().nth(PREVIEW_LINES * PREVIEW_CHARS) {
                    text.truncate(cut);
                    text.push('\u{2026}');
                }
                let rows = text.lines().map(|l| l.chars().count().div_ceil(PREVIEW_CHARS).max(1)).sum::<usize>().clamp(1, PREVIEW_LINES);
                // Matrix keeps only the time the server took it, which is when it was received too.
                let when = format!("{}, {}", day_label(m.ts), clock(m.ts));
                let mut lines = vec![Line::Preview { text, own: m.own, rows }, Line::Fact(if m.own { t!("Sent") } else { t!("Received") }, when)];
                if !m.own {
                    lines.push(Line::Head(t!("Sender")));
                    lines.extend(listed(std::iter::once((m.from.clone(), short_time(m.ts)))));
                }
                // A receipt stands on the newest row its sender has read: each of them has read this one.
                let mut read: Vec<(OwnedUserId, u64)> = Vec::new();
                for (user, ts) in self.msgs[ix..].iter().flat_map(|row| &row.seen) {
                    if *user != m.from && !read.iter().any(|(u, _)| u == user) {
                        read.push((user.clone(), *ts));
                    }
                }
                read.sort_by_cached_key(|(user, _)| self.person_name(user).to_lowercase());
                if !read.is_empty() {
                    lines.push(Line::Head(t!("Read by")));
                    let when = |ts: u64| if ts == 0 { String::new() } else { short_time(ts) };
                    lines.extend(listed(read.iter().map(|(user, ts)| (user.clone(), when(*ts)))));
                }
                if m.own {
                    let rest: Vec<(OwnedUserId, String)> =
                        self.members().iter().filter(|(user, _)| !read.iter().any(|(u, _)| u == user)).map(|(user, _)| (user.clone(), String::new())).collect();
                    if !rest.is_empty() {
                        lines.push(Line::Head(t!("Sent to")));
                        lines.extend(listed(rest.into_iter()));
                    }
                }
                Some((t!("Message details"), Vec::new(), lines))
            }
            Shown::Reactions(id, key) => {
                let m = self.msgs.iter().find(|m| m.id == *id && !m.reactions.is_empty())?;
                let total: usize = m.reactions.iter().map(|r| r.count).sum();
                let mut tabs = vec![(None, t!("All · {n}", n = total))];
                tabs.extend(m.reactions.iter().map(|r| (Some(r.key.clone()), format!("{} \u{b7} {}", r.key, r.count))));
                // A tab whose emoji everyone took back shows them all.
                let key = key.as_ref().filter(|k| m.reactions.iter().any(|r| &r.key == *k));
                let mut lines = Vec::new();
                let mut hidden = 0;
                for r in m.reactions.iter().filter(|r| key.is_none_or(|k| *k == r.key)) {
                    let mut rows: Vec<Line> = Vec::new();
                    if r.own {
                        rows.push(Line::Person { user: None, aside: r.key.clone(), emoji: true, remove: Some(r.key.clone()) });
                    }
                    rows.extend(r.by.iter().map(|user| Line::Person { user: Some(user.clone()), aside: r.key.clone(), emoji: true, remove: None }));
                    hidden += r.count.saturating_sub(rows.len());
                    lines.extend(rows);
                }
                lines.truncate(MAX_ROWS);
                lines.extend((hidden > 0).then_some(Line::More(hidden)));
                Some((t!("Reactions"), tabs, lines))
            }
        }
    }

    pub(super) fn render_people(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.audience.open.is_some() {
            self.audience.last = self.audience.open.clone();
        }
        let pointer = self.audience.at.filter(|_| self.audience.open.is_some());
        let frame = if pointer.is_some() {
            None
        } else {
            let Some(frame) = self.audience.sheet.sample(self.audience.open.is_some(), window, cx) else {
                self.audience.last = None;
                return None;
            };
            Some(frame)
        };
        let shown = self.audience.last.clone()?;
        let Some((title, tabs, lines)) = self.people_lines(&shown) else {
            self.audience.open = None;
            return None;
        };
        for line in &lines {
            if let Line::Person { user: Some(user), .. } = line {
                self.want_face(user.clone());
            }
        }
        let live = frame.is_none_or(|f| f.live);
        let p = Palette::of(cx);
        let tall = |lines: &[Line]| lines.iter().map(Line::h).sum::<f32>() + if tabs.is_empty() { 0. } else { TABS_H } + 8.;
        // As tall as all the reactions take, so it stays put while the tabs change.
        let content_h = match &shown {
            Shown::Reactions(id, Some(_)) => self.people_lines(&Shown::Reactions(id.clone(), None)).map_or_else(|| tall(&lines), |(_, _, all)| tall(&all)),
            Shown::Reactions(..) | Shown::Details(_) => tall(&lines),
        };
        let id = match &shown {
            Shown::Details(id) | Shown::Reactions(id, _) => id.clone(),
        };
        let picked = match &shown {
            Shown::Reactions(_, key) => key.clone(),
            Shown::Details(_) => None,
        };
        let tabs = (!tabs.is_empty()).then(|| {
            h_flex().id("people-tabs").flex_none().h(px(TABS_H)).px(px(GUTTER)).gap(px(6.)).overflow_x_scroll().children(tabs.into_iter().enumerate().map(|(n, (key, label))| {
                let on = key == picked;
                let id = id.clone();
                h_flex()
                    .id(("people-tab", n))
                    .flex_none()
                    .h(px(32.))
                    .px(px(12.))
                    .rounded_full()
                    .text_size(fs(14.))
                    .font_features(tabular())
                    .text_color(if on { p.text } else { p.faint })
                    .when(on, |el| el.bg(p.raised))
                    .cursor_pointer()
                    .child(crate::emoji::styled(label, Vec::new()))
                    .when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| {
                        this.audience.open = Some(Shown::Reactions(id.clone(), key.clone()));
                        cx.notify();
                    })))
            }))
        });
        let rows = lines.into_iter().enumerate().map(|(n, line)| self.people_row(n, line, &id, live, cx));
        let list = v_flex().id("people-list").flex_1().min_h_0().pb(px(8.)).overflow_y_scroll().children(rows);
        if let Some(at) = pointer {
            return Some(self.people_popover(at, title, tabs, list, content_h, window, cx));
        }
        let frame = frame?;
        let (top, bottom) = crate::insets();
        let head = v_flex()
            .id("people-head")
            .flex_none()
            .when(live, |el| {
                el.child(sheet::grip(cx, |this: &mut Self| &mut this.audience.sheet, |this, _| this.audience.open = None))
            })
            .child(sheet::handle(p.faint))
            .child(div().h(px(36.)).px(px(GUTTER + 4.)).flex().items_center().truncate().text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD).child(title));
        let most = (f32::from(frame::viewport(window).height) - top) * 0.85;
        let height = (sheet::HANDLE_H + 36. + content_h + bottom + 8.).min(most).round();
        Some(
            sheet::scrim("people-scrim", frame)
                .when(live, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        this.close_people(cx);
                    }))
                })
                .child(
                    v_flex()
                        .id("people-sheet")
                        .relative()
                        .top(px(frame.y))
                        .w_full()
                        .max_w(px(SHEET_W))
                        .h(px(height))
                        .pb(px(bottom + 8.))
                        .rounded_t(px(RADIUS))
                        .bg(p.surface)
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(self.audience.sheet.probe(cx.entity_id()))
                        .child(head)
                        .children(tabs)
                        .child(list),
                )
                .into_any_element(),
        )
    }

    /// The list where the pointer is, like a message's menu; a click outside closes it.
    #[expect(clippy::too_many_arguments, reason = "the parts of the list, built by the caller")]
    fn people_popover(
        &self,
        at: Point<Pixels>,
        title: &'static str,
        tabs: Option<Stateful<Div>>,
        list: Stateful<Div>,
        content_h: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::of(cx);
        let view = frame::viewport(window);
        let (top, bottom) = crate::insets();
        let head_h = 36.;
        let w = f32::from(fs(POINTER_SHEET_W));
        let (vw, vh) = (f32::from(view.width), f32::from(view.height));
        let h = (head_h + content_h).min(POPOVER_MAX_H).min(vh - top - bottom - 2. * GUTTER);
        let at = frame::local(window, at);
        let (ax, ay) = (f32::from(at.x), f32::from(at.y));
        let x = if ax + w <= vw - GUTTER { ax } else { ax - w };
        let y = if ay + h <= vh - bottom - GUTTER { ay } else { ay - h };
        let x = x.clamp(GUTTER, (vw - w - GUTTER).max(GUTTER));
        let y = y.clamp(top + GUTTER, (vh - bottom - h - GUTTER).max(top + GUTTER));
        let head = div().flex_none().h(px(head_h)).px(px(GUTTER)).flex().items_center().truncate().text_size(fs(13.)).text_color(p.faint).child(title);
        let panel = v_flex().id("people-popover").w(px(w)).h(px(h)).rounded(px(14.)).bg(p.lifted).occlude().child(head).children(tabs).child(list);
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.close_people(cx);
        };
        div()
            .id("people-barrier")
            .absolute()
            .inset_0()
            .occlude()
            .on_click(cx.listener(move |this, _, _, cx| close(this, cx)))
            .on_aux_click(cx.listener(move |this, _, _, cx| close(this, cx)))
            .child(div().absolute().left(px(x)).top(px(y)).child(panel).with_animation(
                SharedString::from(format!("people-{ax}-{ay}")),
                Animation::new(motion::ENTER).with_easing(motion::ease_out),
                move |el, t| el.top(px(y + 4. * (1. - t))).opacity(t),
            ))
            .into_any_element()
    }

    fn people_row(&self, n: usize, line: Line, id: &str, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let edge = || h_flex().w_full().flex_none().px(px(GUTTER + 4.));
        match line {
            Line::Preview { text, own, rows } => {
                let inside = if own { p.in_own_bubble() } else { p };
                edge()
                    .h(px(Line::Preview { text: String::new(), own, rows }.h()))
                    .py(px(8.))
                    .map(|el| if own { el.justify_end() } else { el.justify_start() })
                    .child(
                        div()
                            .max_w(relative(BUBBLE_SHARE))
                            .px(px(GAP))
                            .py(px(9.))
                            .rounded(px(RADIUS))
                            .bg(if own { p.own } else { p.surface })
                            .text_color(inside.text)
                            .text_size(fs(16.))
                            .line_height(fs(22.))
                            .max_h(fs(22.) * rows as f32 + px(2. * 9.))
                            .overflow_hidden()
                            .child(text),
                    )
                    .into_any_element()
            }
            Line::Head(text) => edge().h(px(HEAD_H)).pt(px(8.)).text_size(fs(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child(text).into_any_element(),
            Line::More(n) => edge().h(px(HEAD_H)).text_size(fs(13.)).font_features(tabular()).text_color(p.faint).child(format!("+{n}")).into_any_element(),
            Line::Fact(label, value) => edge()
                .h(px(FACT_H))
                .gap(px(GAP))
                .text_size(fs(14.))
                .child(div().flex_1().min_w_0().truncate().text_color(p.faint).child(label))
                .child(div().flex_none().font_features(tabular()).child(value))
                .into_any_element(),
            Line::Person { user, aside, emoji, remove } => {
                let name = user.as_ref().map_or_else(|| t!("You").to_owned(), |user| self.person_name(user));
                let face = avatar(&name, user.as_ref().and_then(|user| self.faces.get(user)), FACE, cx);
                let hint = remove.is_some().then(|| if gpui_kit::is_mobile() { t!("Tap to remove") } else { t!("Click to remove") });
                let aside = if emoji {
                    div().flex_none().text_size(fs(20.)).child(crate::emoji::styled(aside, Vec::new())).into_any_element()
                } else {
                    div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(aside).into_any_element()
                };
                let row = edge()
                    .id(("people-row", n))
                    .h(px(PERSON_H))
                    .gap(px(12.))
                    .child(face)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(fs(16.)).child(name))
                            .children(hint.map(|h| div().truncate().text_size(fs(12.)).text_color(p.faint).child(h))),
                    )
                    .child(aside);
                match remove.filter(|_| live) {
                    Some(key) => {
                        let id = id.to_owned();
                        row.cursor_pointer()
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                            .active(|el| el.bg(p.raised))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_people(cx);
                                this.toggle_reaction(&id, &key, cx);
                            }))
                            .into_any_element()
                    }
                    None => row.into_any_element(),
                }
            }
        }
    }
}
