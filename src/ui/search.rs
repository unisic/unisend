//! Message search: a bar over the open chat, and a Messages section under the chat list's matches.
//! The backend answers from this device's index (`backend/search.rs`). The chat's bar narrows it
//! to media, links or files, to one sender and to a time; with any of those set, the bar lists
//! what they let through before a word is typed.

use super::*;
use crate::backend::{Filter, Hit, MAX_HITS, MIN_QUERY, Only};
use crate::i18n::t;

/// Older pages a jump to a found message may load before it gives up; 40 events each.
pub(super) const JUMP_PAGES: u8 = 50;

pub(super) struct Find {
    pub(super) input: Entity<InputState>,
    /// The bar over the open chat is showing.
    pub(super) open: bool,
    /// Its results cover the conversation; hidden after a jump until the field is touched again.
    results: bool,
    /// The latest answers, each with the query it answers; at most `MAX_HITS` hits each.
    local: (String, Filter, Vec<Hit>),
    global: (String, Vec<Hit>),
    /// What the chat's search is narrowed to; `when` is the choice `filter`'s times came from.
    filter: Filter,
    when: When,
    /// The choices of a filter, listed in place of the results.
    picking: Option<Picking>,
}

#[derive(Clone, Copy, PartialEq)]
enum Picking {
    From,
    When,
}

/// The times a search can be narrowed to, counted back from when the choice is made.
#[derive(Clone, Copy, PartialEq)]
enum When {
    Any,
    Week,
    Month,
    Year,
    Older,
}

impl When {
    const ALL: [Self; 5] = [Self::Any, Self::Week, Self::Month, Self::Year, Self::Older];

    fn label(self) -> &'static str {
        match self {
            Self::Any => t!("Any time"),
            Self::Week => t!("Past week"),
            Self::Month => t!("Past month"),
            Self::Year => t!("Past year"),
            Self::Older => t!("Older than a year"),
        }
    }

    /// (since, until) in ms since the epoch, for `now`.
    fn range(self, now: u64) -> (Option<u64>, Option<u64>) {
        const DAY: u64 = 24 * 60 * 60 * 1000;
        let back = |days: u64| Some(now.saturating_sub(days * DAY));
        match self {
            Self::Any => (None, None),
            Self::Week => (back(7), None),
            Self::Month => (back(30), None),
            Self::Year => (back(365), None),
            Self::Older => (None, back(365)),
        }
    }
}

impl Find {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search this chat")));
        crate::ime::follow(&input, window, cx);
        subs.push(cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            match ev {
                InputEvent::Change => this.find_in_chat(cx),
                InputEvent::Focus => this.find.results = true,
                _ => {}
            }
            keyboard_follows(ev, Keyboard::Text, cx);
            cx.notify();
        }));
        Self { input, open: false, results: true, local: Default::default(), global: Default::default(), filter: Filter::default(), when: When::Any, picking: None }
    }

    /// The open chat is gone: so is its search.
    pub(super) fn close(&mut self) {
        self.open = false;
        self.local = Default::default();
        (self.filter, self.when, self.picking) = (Filter::default(), When::Any, None);
    }
}

fn query_of(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_owned()
}

fn long_enough(query: &str) -> bool {
    query.chars().count() >= MIN_QUERY
}

impl BeeperApp {
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.close();
        (self.find.open, self.find.results) = (true, true);
        self.find.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.close();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn find_in_chat(&mut self, cx: &mut Context<Self>) {
        let query = query_of(&self.find.input, cx);
        let filter = self.find.filter.clone();
        let Some(room) = self.open.clone().filter(|_| long_enough(&query) || !filter.is_empty()) else {
            self.find.local = (query, filter, Vec::new());
            return;
        };
        let _ = self.cmd.try_send(Cmd::Search { room: Some(room), query, filter });
    }

    fn set_filter(&mut self, change: impl FnOnce(&mut Find), cx: &mut Context<Self>) {
        change(&mut self.find);
        (self.find.picking, self.find.results) = (None, true);
        self.find_in_chat(cx);
        cx.notify();
    }

    fn pick_filter(&mut self, picking: Picking, cx: &mut Context<Self>) {
        if picking == Picking::From {
            self.want_members();
        }
        self.find.picking = (self.find.picking != Some(picking)).then_some(picking);
        self.find.results = true;
        cx.notify();
    }

    /// The chat list's query changed: look for it in messages too.
    pub(super) fn find_everywhere(&mut self, cx: &mut Context<Self>) {
        let query = query_of(&self.search, cx);
        if !self.searching || !long_enough(&query) {
            self.find.global = (query, Vec::new());
            return;
        }
        let _ = self.cmd.try_send(Cmd::Search { room: None, query, filter: Filter::default() });
    }

    /// An answer; one for a query the user has typed past is dropped.
    pub(super) fn found(&mut self, room: Option<OwnedRoomId>, query: String, filter: Filter, hits: Vec<Hit>, cx: &mut Context<Self>) {
        match room {
            None if self.searching && filter.is_empty() && query == query_of(&self.search, cx) => {
                let missing: Vec<OwnedRoomId> =
                    hits.iter().map(|h| h.room.clone()).filter(|id| !self.avatars.contains_key(id)).collect::<HashSet<_>>().into_iter().collect();
                if !missing.is_empty() && self.cmd.try_send(Cmd::Avatars(missing.clone())).is_ok() {
                    self.asked_avatars(missing, cx);
                }
                self.find.global = (query, hits);
            }
            Some(room) if self.find.open && self.open.as_ref() == Some(&room) && query == query_of(&self.find.input, cx) && filter == self.find.filter => {
                self.find.local = (query, filter, hits);
            }
            _ => {}
        }
    }

    pub(super) fn jump(&mut self, room: OwnedRoomId, id: String, cx: &mut Context<Self>) {
        if self.open.as_ref() == Some(&room) {
            self.reveal_within(&id, JUMP_PAGES, cx);
            return;
        }
        self.open_room(room, cx);
        // Looked for as the rows arrive, in place of the first unread one.
        self.place.seek_unread = false;
        self.seek = Some((id, JUMP_PAGES));
    }

    /// Rows the chat list holds after the chats: a heading and the messages found.
    pub(super) fn message_rows(&self) -> usize {
        match self.find.global.1.len() {
            0 => 0,
            n => n + 1,
        }
    }

    /// Row `ix` of the Messages section, as tall as a chat row: the list measures one row for all.
    pub(super) fn message_row(&self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let Some(hit) = ix.checked_sub(1).and_then(|ix| self.find.global.1.get(ix)) else {
            return h_flex()
                .id("messages-heading")
                .w_full()
                .h(px(ROOM_ROW_H))
                .px(px(GUTTER))
                .pb(px(8.))
                .items_end()
                .text_size(fs(14.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.faint)
                .child(t!("Messages"));
        };
        let room = self.rooms.iter().find(|r| r.id == hit.room);
        let name = room.map_or_else(|| hit.room.to_string(), |r| r.name.clone());
        let dm = room.is_some_and(|r| r.dm);
        let prefix = match (hit.own, dm) {
            (true, _) => t!("You: ").to_owned(),
            (false, true) => String::new(),
            (false, false) => format!("{}: ", hit.sender),
        };
        let (line, marks) = with_prefix(&prefix, hit, p);
        let (room_id, id) = (hit.room.clone(), hit.id.clone());
        h_flex()
            .id(SharedString::from(format!("hit-{}", hit.id)))
            .w_full()
            .h(px(ROOM_ROW_H))
            .px(px(GUTTER))
            .gap(px(GAP))
            .cursor_pointer()
            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
            .active(|el| el.bg(p.raised))
            .on_click(cx.listener(move |this, _, _, cx| this.jump(room_id.clone(), id.clone(), cx)))
            .child(avatar(&name, self.avatars.get(&hit.room), AVATAR, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().truncate().text_size(fs(16.)).line_height(fs(22.)).child(name))
                            .child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(short_time(hit.ts))),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(fs(14.))
                            .line_height(fs(20.))
                            .text_color(p.muted)
                            .child(crate::emoji::styled(line, marks)),
                    ),
            )
    }

    pub(super) fn find_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        round_button("find", IconName::Search, t!("Find in chat"), cx).on_click(cx.listener(|this, _, window, cx| this.open_find(window, cx)))
    }

    /// The search field in place of the chat's header, and the filters under it.
    pub(super) fn find_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let p = Palette::of(cx);
        self.find.open.then(|| {
            let field = h_flex().flex_none().h(px(BAR_H)).px(px(GUTTER)).child(
                h_flex()
                    .flex_1()
                    .h(px(TAP))
                    .pl(px(GAP))
                    .pr(px(4.))
                    .rounded_full()
                    .bg(p.surface)
                    .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.find.input).appearance(false)).child(crate::ime::marks(&self.find.input)))
                    .child(round_button("close-find", IconName::X, t!("Close search"), cx).on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx)))),
            );
            v_flex().flex_none().child(field).child(self.find_filters(cx))
        })
    }

    /// Media, Links, Files, From and Date, as chips; From and Date list their choices.
    fn find_filters(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let filter = &self.find.filter;
        let kinds = [(Only::Media, t!("Media")), (Only::Links, t!("Links")), (Only::Files, t!("Files"))].map(|(only, label)| {
            let on = filter.only == only;
            chip(("find-only", only as usize), label.to_owned(), on, false, cx).on_click(cx.listener(move |this, _, _, cx| {
                this.set_filter(|find| find.filter.only = if on { Only::All } else { only }, cx);
            }))
        });
        let from = filter.from.as_ref().map(|user| self.sender_name(user));
        let when = (self.find.when != When::Any).then(|| self.find.when.label().to_owned());
        h_flex()
            .id("find-filters")
            .px(px(GUTTER))
            .pb(px(8.))
            .gap(px(8.))
            .overflow_x_scroll()
            .children(kinds)
            .child(
                chip("find-from", from.clone().unwrap_or_else(|| t!("From").to_owned()), from.is_some(), true, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_filter(Picking::From, cx))),
            )
            .child(
                chip("find-when", when.clone().unwrap_or_else(|| t!("Date").to_owned()), when.is_some(), true, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_filter(Picking::When, cx))),
            )
    }

    fn sender_name(&self, user: &OwnedUserId) -> String {
        if self.user.as_ref() == Some(user) {
            return t!("You").to_owned();
        }
        self.members().iter().find(|(id, _)| id == user).map_or_else(|| user.to_string(), |(_, name)| name.clone())
    }

    /// The choices of the filter being picked, in place of the results.
    fn find_choices(&self, picking: Picking, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        match picking {
            Picking::When => When::ALL
                .into_iter()
                .enumerate()
                .map(|(ix, when)| {
                    choice(("find-when-choice", ix), when.label().to_owned(), self.find.when == when, cx).on_click(cx.listener(move |this, _, _, cx| {
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                        this.set_filter(
                            |find| {
                                find.when = when;
                                (find.filter.since, find.filter.until) = when.range(now);
                            },
                            cx,
                        );
                    }))
                })
                .collect(),
            Picking::From => {
                let people = std::iter::once((None, t!("Anyone").to_owned()))
                    .chain(self.user.clone().map(|me| (Some(me), t!("You").to_owned())))
                    .chain(self.members().iter().map(|(id, name)| (Some(id.clone()), name.clone())));
                people
                    .enumerate()
                    .map(|(ix, (user, name))| {
                        let on = self.find.filter.from == user;
                        choice(("find-from-choice", ix), name, on, cx).on_click(cx.listener(move |this, _, _, cx| {
                            let user = user.clone();
                            this.set_filter(|find| find.filter.from = user, cx);
                        }))
                    })
                    .collect()
            }
        }
    }

    /// What was found in the open chat, over its messages.
    pub(super) fn find_results(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let query = query_of(&self.find.input, cx);
        if !self.find.open || !self.find.results || (!long_enough(&query) && self.find.filter.is_empty() && self.find.picking.is_none()) {
            return None;
        }
        let p = Palette::of(cx);
        if let Some(picking) = self.find.picking {
            return Some(div().id("find-choices").absolute().inset_0().bg(p.ground).overflow_y_scroll().child(v_flex().w_full().py(px(4.)).children(self.find_choices(picking, cx))));
        }
        let (answered, filter, hits) = &self.find.local;
        let note = |text: &'static str| div().px(px(GUTTER)).py(px(GAP)).text_size(fs(14.)).text_color(p.faint).child(text);
        let rows = hits.iter().map(|hit| {
            let (line, marks) = with_prefix("", hit, p);
            let id = hit.id.clone();
            v_flex()
                .id(SharedString::from(format!("found-{}", hit.id)))
                .w_full()
                .min_h(px(TAP))
                .px(px(GUTTER))
                .py(px(10.))
                .gap(px(2.))
                .cursor_pointer()
                .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
                .active(|el| el.bg(p.raised))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.find.results = false;
                    window.focus(&this.focus, cx);
                    this.reveal_within(&id, JUMP_PAGES, cx);
                }))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(fs(16.))
                                .line_height(fs(22.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(if hit.own { t!("You").to_owned() } else { hit.sender.clone() }),
                        )
                        .child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(short_time(hit.ts))),
                )
                .child(
                    div()
                        .line_clamp(2)
                        .text_ellipsis()
                        .text_size(fs(14.))
                        .line_height(fs(20.))
                        .text_color(p.muted)
                        .child(crate::emoji::styled(line, marks)),
                )
        });
        let waiting = hits.is_empty() && (*answered != query || *filter != self.find.filter);
        Some(
            div()
                .id("find-results")
                .absolute()
                .inset_0()
                .bg(p.ground)
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .w_full()
                        .children(rows)
                        .when(hits.is_empty() && !waiting, |el| el.child(note(t!("No messages match"))))
                        .when(hits.len() == MAX_HITS, |el| el.child(note(t!("Showing the newest matches. Type more to narrow them down."))))
                        .when(!waiting, |el| el.child(note(t!("Searches the messages this device has received or loaded.")))),
                ),
        )
    }
}

/// The snippet after `prefix`, with its matches set in the text color; for media without a
/// caption, what it is.
fn with_prefix(prefix: &str, hit: &Hit, p: Palette) -> (String, Vec<(std::ops::Range<usize>, HighlightStyle)>) {
    let style = HighlightStyle { color: Some(p.text), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
    let shift = prefix.len();
    let marks = hit.marks.iter().map(|r| (r.start + shift..r.end + shift, style)).collect();
    let text = match (hit.snippet.is_empty(), hit.media) {
        (true, Some(kind)) => match kind {
            MediaKind::Image => t!("Photo"),
            MediaKind::Video => t!("Video"),
            MediaKind::Sticker => t!("Sticker"),
            MediaKind::Voice => t!("Voice message"),
            MediaKind::Audio => t!("Audio"),
            MediaKind::File => t!("File"),
        },
        _ => hit.snippet.as_str(),
    };
    (format!("{prefix}{text}"), marks)
}

/// A filter's chip; `list` when a tap lists choices rather than turning it on.
fn chip(id: impl Into<ElementId>, label: String, on: bool, list: bool, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .flex_none()
        .h(px(32.))
        .px(px(12.))
        .gap(px(4.))
        .rounded_full()
        .border_1()
        .border_color(if on { p.raised } else { p.rule })
        .when(on, |el| el.bg(p.raised))
        .text_size(fs(14.))
        .text_color(if on { p.text } else { p.muted })
        .role(Role::Button)
        .aria_label(label.clone())
        .when(!list, |el| el.aria_toggled(on.into()))
        .cursor_pointer()
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
        .active(|el| el.bg(p.raised))
        .child(label)
        .when(list, |el| el.child(Icon::new(IconName::ChevronDown).size_4().text_color(p.faint)))
}

/// One choice of a filter, checked when it is the one set.
fn choice(id: impl Into<ElementId>, label: String, on: bool, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .w_full()
        .min_h(px(TAP))
        .px(px(GUTTER))
        .gap(px(GAP))
        .role(Role::RadioButton)
        .aria_label(label.clone())
        .aria_toggled(on.into())
        .cursor_pointer()
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
        .active(|el| el.bg(p.raised))
        .child(div().flex_1().min_w_0().truncate().text_size(fs(16.)).child(label))
        .when(on, |el| el.child(Icon::new(IconName::Check).size_5().flex_none().text_color(p.text)))
}

#[cfg(test)]
mod tests {
    use super::When;

    #[test]
    fn times_count_back_from_the_choice() {
        const DAY: u64 = 24 * 60 * 60 * 1000;
        let now = 1000 * DAY;
        assert_eq!(When::Any.range(now), (None, None));
        assert_eq!(When::Week.range(now), (Some(993 * DAY), None));
        assert_eq!(When::Older.range(now), (None, Some(635 * DAY)));
        assert_eq!(When::Year.range(DAY), (Some(0), None), "never before the epoch");
    }
}
