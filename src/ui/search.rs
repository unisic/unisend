//! Message search: a bar over the open chat, and a Messages section under the chat list's matches.
//! The backend answers from this device's index (`backend/search.rs`).

use super::*;
use crate::backend::{Hit, MAX_HITS, MIN_QUERY};
use crate::i18n::t;

/// Older pages a jump to a found message may load before it gives up; 40 events each.
const JUMP_PAGES: u8 = 50;

pub(super) struct Find {
    pub(super) input: Entity<InputState>,
    /// The bar over the open chat is showing.
    pub(super) open: bool,
    /// Its results cover the conversation; hidden after a jump until the field is touched again.
    results: bool,
    /// The latest answers, each with the query it answers; at most `MAX_HITS` hits each.
    local: (String, Vec<Hit>),
    global: (String, Vec<Hit>),
}

impl Find {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search this chat")));
        subs.push(cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            match ev {
                InputEvent::Change => this.find_in_chat(cx),
                InputEvent::Focus => this.find.results = true,
                _ => {}
            }
            keyboard_follows(ev, Keyboard::Text, cx);
            cx.notify();
        }));
        Self { input, open: false, results: true, local: Default::default(), global: Default::default() }
    }

    /// The open chat is gone: so is its search.
    pub(super) fn close(&mut self) {
        self.open = false;
        self.local = Default::default();
    }
}

fn query_of(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_owned()
}

fn long_enough(query: &str) -> bool {
    query.chars().count() >= MIN_QUERY
}

impl BeeperApp {
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let Some(room) = self.open.clone().filter(|_| long_enough(&query)) else {
            self.find.local = (query, Vec::new());
            return;
        };
        let _ = self.cmd.try_send(Cmd::Search { room: Some(room), query });
    }

    /// The chat list's query changed: look for it in messages too.
    pub(super) fn find_everywhere(&mut self, cx: &mut Context<Self>) {
        let query = query_of(&self.search, cx);
        if !self.searching || !long_enough(&query) {
            self.find.global = (query, Vec::new());
            return;
        }
        let _ = self.cmd.try_send(Cmd::Search { room: None, query });
    }

    /// An answer; one for a query the user has typed past is dropped.
    pub(super) fn found(&mut self, room: Option<OwnedRoomId>, query: String, hits: Vec<Hit>, cx: &mut Context<Self>) {
        match room {
            None if self.searching && query == query_of(&self.search, cx) => {
                let missing: Vec<OwnedRoomId> =
                    hits.iter().map(|h| h.room.clone()).filter(|id| !self.avatars.contains_key(id)).collect::<HashSet<_>>().into_iter().collect();
                if !missing.is_empty() && self.cmd.try_send(Cmd::Avatars(missing.clone())).is_ok() {
                    self.avatars.extend(missing.into_iter().map(|id| (id, None)));
                }
                self.find.global = (query, hits);
            }
            Some(room) if self.find.open && self.open.as_ref() == Some(&room) && query == query_of(&self.find.input, cx) => {
                self.find.local = (query, hits);
            }
            _ => {}
        }
    }

    fn jump(&mut self, room: OwnedRoomId, id: String, cx: &mut Context<Self>) {
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
        round_button("find", IconName::Search, cx).on_click(cx.listener(|this, _, window, cx| this.open_find(window, cx)))
    }

    /// The search field in place of the chat's header.
    pub(super) fn find_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let p = Palette::of(cx);
        self.find.open.then(|| {
            h_flex().flex_none().h(px(BAR_H)).px(px(GUTTER)).child(
                h_flex()
                    .flex_1()
                    .h(px(TAP))
                    .pl(px(GAP))
                    .pr(px(4.))
                    .rounded_full()
                    .bg(p.surface)
                    .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.find.input).appearance(false)))
                    .child(round_button("close-find", IconName::X, cx).on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx)))),
            )
        })
    }

    /// What was found in the open chat, over its messages.
    pub(super) fn find_results(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let query = query_of(&self.find.input, cx);
        if !self.find.open || !self.find.results || !long_enough(&query) {
            return None;
        }
        let p = Palette::of(cx);
        let (answered, hits) = &self.find.local;
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
        let waiting = hits.is_empty() && *answered != query;
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

/// The snippet after `prefix`, with its matches set in the text colour.
fn with_prefix(prefix: &str, hit: &Hit, p: Palette) -> (String, Vec<(std::ops::Range<usize>, HighlightStyle)>) {
    let style = HighlightStyle { color: Some(p.text), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
    let shift = prefix.len();
    let marks = hit.marks.iter().map(|r| (r.start + shift..r.end + shift, style)).collect();
    (format!("{prefix}{}", hit.snippet), marks)
}
