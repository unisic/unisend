//! Forwarding, selecting several messages, and deleting a message only on this device.
//!
//! Forward opens a sheet of chats, newest first, to pick up to five; the content is sent again
//! by the backend (`backend/forward.rs`). Select puts the chat in selection mode: a tap toggles a
//! message and a bar over the chat's header acts on all of them. "Delete for me" hides a message
//! on this device only, remembered per account; "Delete for everyone" is the redaction.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use super::sheet::{self, SheetMotion};
use super::*;
use crate::backend::forward::{MAX_CHATS, MAX_MESSAGES};
use crate::i18n::{t, tn};

/// Messages hidden on this device, per account; the oldest is forgotten past this.
const MAX_HIDDEN: usize = 5000;
/// Chats listed in the forward sheet at once; a search finds the rest.
const PICKER_ROWS: usize = 50;
const TOAST_FOR: Duration = Duration::from_secs(5);
const MARK: f32 = 22.;

/// Message IDs hidden on this device, oldest first, with a file to keep them in.
#[derive(Default)]
pub(super) struct Hidden {
    order: VecDeque<String>,
    set: HashSet<String>,
    path: Option<PathBuf>,
}

impl Hidden {
    /// The account's list in `dir`: one event ID per line.
    pub fn load(dir: &Path, user: &str) -> Self {
        let name: String = user.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        let path = dir.join(format!("hidden-{name}.txt"));
        let mut hidden = Self { path: Some(path.clone()), ..Self::default() };
        if let Ok(text) = std::fs::read_to_string(&path) {
            hidden.add(text.lines().filter(|l| l.starts_with('$')).map(str::to_owned));
        }
        hidden
    }

    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    fn add(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if self.set.insert(id.clone()) {
                self.order.push_back(id);
            }
        }
        while self.order.len() > MAX_HIDDEN {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
    }

    fn remove(&mut self, ids: &[String]) {
        self.set.retain(|id| !ids.contains(id));
        self.order.retain(|id| !ids.contains(id));
    }

    /// Writes the whole list: at most 5000 lines, about 250 kB.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut text = String::with_capacity(self.order.len() * 48);
        for id in &self.order {
            text.push_str(id);
            text.push('\n');
        }
        if let Err(e) = std::fs::write(path, text) {
            log::warn!("hidden messages: {e}");
        }
    }
}

#[derive(Clone)]
struct Toast {
    text: SharedString,
    /// Hidden messages the toast's Undo shows again.
    undo: Option<Vec<String>>,
    epoch: u64,
}

pub(super) struct Forward {
    /// Selection mode in a chat, with the messages chosen in the order they were tapped.
    selected: Option<(OwnedRoomId, Vec<String>)>,
    /// Delete for everyone was tapped once; a second tap deletes.
    armed: bool,
    /// The forward sheet: the chat the messages are in and the messages, oldest first.
    picking: Option<(OwnedRoomId, Vec<String>)>,
    picker_last: Option<(OwnedRoomId, Vec<String>)>,
    sheet: SheetMotion,
    chosen: Vec<OwnedRoomId>,
    search: Entity<InputState>,
    pub hidden: Hidden,
    toast: Option<Toast>,
    toast_last: Option<Toast>,
    epoch: u64,
    _sub: Subscription,
}

impl Forward {
    pub fn new(window: &mut Window, cx: &mut Context<BeeperApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search chats")));
        let sub = cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => cx.notify(),
            other => keyboard_follows(other, Keyboard::Text, cx),
        });
        Self {
            selected: None,
            armed: false,
            picking: None,
            picker_last: None,
            sheet: SheetMotion::default(),
            chosen: Vec::new(),
            search,
            hidden: Hidden::default(),
            toast: None,
            toast_last: None,
            epoch: 0,
            _sub: sub,
        }
    }

    pub fn selecting(&self) -> bool {
        self.selected.is_some()
    }
}

/// A message whose content can be sent again: on the server, readable, and not a poll.
pub(super) fn forwardable(m: &Msg) -> bool {
    selectable(m) && !m.deleted && m.poll.is_none() && m.body != crate::backend::utd()
}

fn selectable(m: &Msg) -> bool {
    m.kind == Kind::Message && m.id.starts_with('$')
}

/// What Copy puts on the clipboard for several messages: each on its own line, with its sender
/// and time.
fn transcript(msgs: &[&Msg]) -> String {
    msgs.iter()
        .map(|m| {
            let at = local(m.ts).map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
            format!("[{at}] {}: {}", m.sender, m.summary())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl BeeperApp {
    pub(super) fn is_hidden(&self, id: &str) -> bool {
        self.fwd.hidden.contains(id)
    }

    /// Opens the forward sheet for these messages of the open chat.
    pub(super) fn forward_start(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let mut ids: Vec<String> = self.msgs.iter().filter(|m| ids.contains(&m.id) && forwardable(m)).map(|m| m.id.clone()).collect();
        ids.truncate(MAX_MESSAGES);
        if ids.is_empty() {
            return;
        }
        self.fwd.chosen.clear();
        self.fwd.search.update(cx, |input, cx| input.set_value("", window, cx));
        self.fwd.picking = Some((room, ids));
        cx.notify();
    }

    fn forward_send(&mut self, cx: &mut Context<Self>) {
        let Some((room, targets)) = self.fwd.picking.take() else { return };
        let to = std::mem::take(&mut self.fwd.chosen);
        if self.send(Cmd::Forward { room, targets, to }, cx) {
            self.fwd.selected = None;
        }
    }

    pub(super) fn select_start(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        self.fwd.selected = Some((room, vec![id.to_owned()]));
        self.fwd.armed = false;
        cx.notify();
    }

    fn select_toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some((_, ids)) = &mut self.fwd.selected else { return };
        if let Some(ix) = ids.iter().position(|i| i == id) {
            ids.remove(ix);
        } else if ids.len() < MAX_MESSAGES {
            ids.push(id.to_owned());
        }
        self.fwd.armed = false;
        cx.notify();
    }

    /// Escape and Android's back, for the forward sheet over the chat.
    pub(super) fn close_forward_sheet(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.fwd.picking.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// Escape and Android's back in selection mode: Delete for everyone's second step first.
    pub(super) fn selection_back(&mut self, cx: &mut Context<Self>) -> bool {
        if !std::mem::take(&mut self.fwd.armed) && self.fwd.selected.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    /// Closes the forward sheet, or else leaves selection mode.
    pub(super) fn forward_back(&mut self, cx: &mut Context<Self>) -> bool {
        if self.fwd.picking.take().is_some() {
        } else if self.fwd.selected.take().is_some() {
            self.fwd.armed = false;
        } else {
            return false;
        }
        cx.notify();
        true
    }

    /// The selected messages that are still in the open chat, in the chat's order.
    fn selection(&self) -> Vec<&Msg> {
        match &self.fwd.selected {
            Some((room, ids)) if self.open.as_ref() == Some(room) => {
                self.msgs.iter().filter(|m| ids.contains(&m.id) && !self.is_hidden(&m.id)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Hides messages on this device, with a way to undo it for a few seconds.
    pub(super) fn hide_for_me(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        self.fwd.hidden.add(ids.iter().cloned());
        self.fwd.hidden.save();
        // A hidden row and its neighbours change height: a run's corners and time move.
        self.timeline.remeasure();
        let text = tn!(ids.len(), "{n} message deleted for you", "{n} messages deleted for you").into();
        self.toast(text, Some(ids), cx);
    }

    fn undo_hide(&mut self, cx: &mut Context<Self>) {
        let Some(ids) = self.fwd.toast.take().and_then(|t| t.undo) else { return };
        self.fwd.hidden.remove(&ids);
        self.fwd.hidden.save();
        self.timeline.remeasure();
        cx.notify();
    }

    fn toast(&mut self, text: SharedString, undo: Option<Vec<String>>, cx: &mut Context<Self>) {
        self.fwd.epoch += 1;
        let epoch = self.fwd.epoch;
        self.fwd.toast = Some(Toast { text, undo, epoch });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.fwd.toast.as_ref().is_some_and(|t| t.epoch == epoch) {
                    this.fwd.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn forwarded(&mut self, chats: usize, cx: &mut Context<Self>) {
        let text = tn!(chats, "Forwarded to {n} chat", "Forwarded to {n} chats").into();
        self.toast(text, None, cx);
    }

    /// The account changed: its own list of hidden messages.
    pub(super) fn load_hidden(&mut self, user: &OwnedUserId) {
        self.fwd.hidden = Hidden::load(&self.dir, user.as_str());
    }

    /// Selection mode's bar, laid over the chat's header.
    pub(super) fn selection_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.fwd.selected.as_ref().is_some_and(|(room, _)| self.open.as_ref() != Some(room)) {
            self.fwd.selected = None;
        }
        let shown = Presence::new("select-bar", self.fwd.selecting()).transition(if self.fwd.selecting() { motion::enter() } else { motion::exit() }).sample(window, cx);
        if !shown.should_render() || !self.fwd.selecting() {
            return None;
        }
        let p = Palette::of(cx);
        let picked = self.selection();
        let ids: Vec<String> = picked.iter().map(|m| m.id.clone()).collect();
        let text = transcript(&picked);
        let can_forward = !picked.is_empty() && picked.iter().all(|m| forwardable(m));
        let for_everyone = !picked.is_empty() && picked.iter().all(|m| m.own && !m.deleted);
        let count = match picked.len() {
            0 => t!("Select messages").to_owned(),
            n => t!("{n} selected", n = n),
        };
        let armed = self.fwd.armed && for_everyone;
        let (forward_ids, hide_ids) = (ids.clone(), ids.clone());
        let bar = h_flex()
            .id("select-bar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(BAR_H))
            .px(px(GUTTER - 10.))
            .gap(px(4.))
            .items_center()
            .bg(p.ground)
            .occlude()
            .opacity(shown.progress)
            .child(round_button("select-close", IconName::X, cx).on_click(cx.listener(|this, _, _, cx| {
                this.forward_back(cx);
            })))
            .child(div().flex_1().min_w_0().pl(px(8.)).truncate().text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD).font_features(tabular()).child(count))
            .when(!picked.is_empty() && !armed, |el| {
                el.child(round_button("select-copy", IconName::Copy, cx).on_click(cx.listener(move |this, _, _, cx| {
                    crate::copy(cx, text.clone());
                    this.forward_back(cx);
                })))
                .when(can_forward, |el| {
                    el.child(round_button("select-forward", IconName::Forward, cx).on_click(cx.listener(move |this, _, window, cx| {
                        this.forward_start(forward_ids.clone(), window, cx)
                    })))
                })
                .child(round_button("select-hide", IconName::EyeOff, cx).on_click(cx.listener(move |this, _, _, cx| {
                    this.hide_for_me(hide_ids.clone(), cx);
                    this.forward_back(cx);
                })))
            })
            .when(for_everyone && !armed, |el| {
                el.child(round_button("select-delete", IconName::Trash, cx).text_color(p.negative).on_click(cx.listener(|this, _, _, cx| {
                    this.fwd.armed = true;
                    cx.notify();
                })))
            })
            .when(armed, |el| {
                // Irreversible for everyone in the chat, so it takes a second, explicit tap.
                el.child(
                    div()
                        .id("select-delete-confirm")
                        .h(px(TAP))
                        .px(px(GUTTER))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(p.raised)
                        .text_color(p.negative)
                        .text_size(fs(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child(t!("Delete for everyone"))
                        .on_click(cx.listener(move |this, _, _, cx| this.delete_for_everyone(ids.clone(), cx))),
                )
            });
        Some(bar.into_any_element())
    }

    fn delete_for_everyone(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let cmd = self.cmd.clone();
        // Through the bounded channel at its pace, so a hundred deletions never overflow it.
        cx.background_spawn(async move {
            for target in ids {
                if cmd.send(Cmd::Delete { room: room.clone(), target }).await.is_err() {
                    break;
                }
            }
        })
        .detach();
        self.forward_back(cx);
    }

    /// Over a message in selection mode: takes its taps, tints it when chosen and shows a check on
    /// the side its bubble leaves free.
    pub(super) fn selection_mark(&self, m: &Msg, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.fwd.selecting() || !selectable(m) {
            return None;
        }
        let p = Palette::of(cx);
        let on = self.fwd.selected.as_ref().is_some_and(|(_, ids)| ids.contains(&m.id));
        let id = m.id.clone();
        let mark = div()
            .size(px(MARK))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .map(|el| if on { el.bg(p.text).text_color(p.ground).child(Icon::new(IconName::Check).size_4()) } else { el.border_2().border_color(p.faint) });
        Some(
            div()
                .id(SharedString::from(format!("select-{}", m.id)))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .px(px(GUTTER))
                .map(|el| if m.own { el.justify_start() } else { el.justify_end() })
                .when(on, |el| el.bg(p.text.opacity(0.08)))
                .block_mouse_except_scroll()
                .cursor_pointer()
                .child(mark)
                .on_click(cx.listener(move |this, _, _, cx| this.select_toggle(&id, cx)))
                .into_any_element(),
        )
    }

    /// The forward sheet and the toast, over everything else.
    pub(super) fn render_forward_layers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        out.extend(self.render_picker_sheet(window, cx));
        out.extend(self.render_toast(window, cx));
        out
    }

    fn render_picker_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.fwd.picking.is_some() {
            self.fwd.picker_last = self.fwd.picking.clone();
        }
        let Some(frame) = self.fwd.sheet.sample(self.fwd.picking.is_some(), window, cx) else {
            self.fwd.picker_last = None;
            return None;
        };
        let live = frame.live;
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let height = ((f32::from(window.viewport_size().height) - top) * 0.85).round();
        let n = self.fwd.picker_last.as_ref().map_or(0, |(_, ids)| ids.len());
        let query = self.fwd.search.read(cx).value().trim().to_lowercase();
        let mut found: Vec<&RoomSummary> = self.rooms.iter().filter(|r| query.is_empty() || r.name.to_lowercase().contains(&query)).collect();
        found.sort_by_key(|r| std::cmp::Reverse(r.ts));
        let full = self.fwd.chosen.len() >= MAX_CHATS;
        let rows: Vec<Stateful<Div>> = found
            .into_iter()
            .take(PICKER_ROWS)
            .map(|r| {
                let on = self.fwd.chosen.contains(&r.id);
                let id = r.id.clone();
                let open = live && (on || !full);
                h_flex()
                    .id(SharedString::from(format!("forward-{}", r.id)))
                    .h(px(SHEET_ROW_H + 8.))
                    .px(px(GUTTER + 4.))
                    .gap(px(GAP))
                    .flex_none()
                    .when(!open && !on, |el| el.opacity(0.4))
                    .when(open, |el| el.cursor_pointer().active(|el| el.bg(p.raised)))
                    .child(avatar(&r.name, self.avatars.get(&r.id), 40., cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(fs(16.)).child(r.name.clone()))
                            .children(r.network.clone().map(|n| div().truncate().text_size(fs(12.)).text_color(p.faint).child(n))),
                    )
                    .child(
                        div()
                            .size(px(MARK))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .map(|el| if on { el.bg(p.text).text_color(p.ground).child(Icon::new(IconName::Check).size_4()) } else { el.border_2().border_color(p.faint) }),
                    )
                    .when(open, |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            match this.fwd.chosen.iter().position(|c| *c == id) {
                                Some(ix) => drop(this.fwd.chosen.remove(ix)),
                                None if this.fwd.chosen.len() < MAX_CHATS => this.fwd.chosen.push(id.clone()),
                                None => {}
                            }
                            cx.notify();
                        }))
                    })
            })
            .collect();
        let chosen = self.fwd.chosen.len();
        let title = match n {
            1 => t!("Forward message").to_owned(),
            n => tn!(n, "Forward {n} message", "Forward {n} messages"),
        };
        let hint = t!("{chosen} of {max} chats", chosen = chosen, max = MAX_CHATS);
        let send_label = tn!(chosen, "Send to {n} chat", "Send to {n} chats");
        let send = Presence::new("forward-send", chosen > 0).transition(if chosen > 0 { motion::enter() } else { motion::exit() }).sample(window, cx);
        Some(
            sheet::scrim("forward-scrim", frame)
                .when(live, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        this.fwd.picking = None;
                        cx.notify();
                    }))
                })
                .child(
                    v_flex()
                        .id("forward-sheet")
                        .relative()
                        .top(px(frame.y))
                        .w_full()
                        .max_w(px(SHEET_W))
                        .h(px(height))
                        .pb(px(bottom + 8.))
                        .rounded_t(px(RADIUS))
                        .bg(p.surface)
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(self.fwd.sheet.probe(cx.entity_id()))
                        .child(
                            v_flex()
                                .id("forward-head")
                                .flex_none()
                                .when(live, |el| {
                                    el.on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                                        if this.fwd.sheet.drag(e) {
                                            this.fwd.picking = None;
                                        }
                                        cx.notify();
                                    }))
                                })
                                .child(sheet::handle(p.faint))
                                .child(
                                    h_flex()
                                        .h(px(36.))
                                        .px(px(GUTTER + 4.))
                                        .gap(px(8.))
                                        .child(div().flex_1().min_w_0().truncate().text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD).child(title))
                                        .child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(hint)),
                                ),
                        )
                        .child(
                            div()
                                .mx(px(GUTTER))
                                .my(px(8.))
                                .min_h(px(TAP))
                                .px(px(GUTTER - 4.))
                                .flex()
                                .items_center()
                                .rounded(px(TAP / 2.))
                                .bg(p.ground)
                                .child(Input::new(&self.fwd.search).appearance(false)),
                        )
                        .child(div().id("forward-chats").flex_1().min_h_0().overflow_y_scroll().children(rows))
                        .when(send.should_render(), |el| {
                            el.child(div().flex_none().overflow_hidden().h(px((TAP + 8.) * send.progress)).opacity(send.progress).child(
                                div()
                                    .id("forward-send")
                                    .mx(px(GUTTER))
                                    .mt(px(8.))
                                    .h(px(TAP))
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(p.text)
                                    .text_color(p.ground)
                                    .text_size(fs(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .child(send_label)
                                    .when(live && chosen > 0, |el| el.on_click(cx.listener(|this, _, _, cx| this.forward_send(cx)))),
                            ))
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.fwd.toast.is_some();
        if live {
            self.fwd.toast_last = self.fwd.toast.clone();
        }
        let key = self.fwd.toast_last.as_ref().map_or(0, |t| t.epoch);
        let shown = Presence::new((("toast", key as usize), String::new()), live).transition(if live { motion::enter() } else { motion::exit() }).sample(window, cx);
        if !shown.should_render() {
            self.fwd.toast_last = None;
            return None;
        }
        let toast = self.fwd.toast_last.clone()?;
        let p = Palette::of(cx);
        let (_, bottom) = crate::insets();
        let v = shown.progress;
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(bottom + 80. - 8. * (1. - v)))
                .flex()
                .justify_center()
                .px(px(GUTTER))
                .opacity(v)
                .child(
                    h_flex()
                        .id("toast")
                        .occlude()
                        .min_h(px(TAP))
                        .pl(px(GUTTER))
                        .pr(px(if toast.undo.is_some() { 4. } else { GUTTER }))
                        .gap(px(8.))
                        .rounded_full()
                        .bg(p.raised)
                        .text_size(fs(14.))
                        .child(toast.text.clone())
                        .when(toast.undo.is_some() && live, |el| {
                            el.child(
                                div()
                                    .id("toast-undo")
                                    .h(px(TAP))
                                    .px(px(GUTTER))
                                    .flex()
                                    .items_center()
                                    .rounded_full()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .cursor_pointer()
                                    .active(|el| el.bg(p.surface))
                                    .child(t!("Undo"))
                                    .on_click(cx.listener(|this, _, _, cx| this.undo_hide(cx))),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Hidden;

    #[test]
    fn hidden_messages_are_kept_per_account_and_the_oldest_go_first() {
        let dir = std::env::temp_dir().join(format!("murmur-hidden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = Hidden::load(&dir, "@a:x.org");
        a.add((0..super::MAX_HIDDEN + 2).map(|n| format!("${n}")));
        a.save();
        let a = Hidden::load(&dir, "@a:x.org");
        assert_eq!(a.order.len(), super::MAX_HIDDEN);
        assert!(!a.contains("$0") && !a.contains("$1") && a.contains("$2"));
        assert!(!Hidden::load(&dir, "@b:x.org").contains("$2"), "another account's list is its own");
        let mut a = a;
        a.remove(&["$2".to_owned()]);
        assert!(!a.contains("$2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
