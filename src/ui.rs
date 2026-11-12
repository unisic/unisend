use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, h_flex,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use matrix_sdk::ruma::OwnedRoomId;
use tokio::sync::mpsc;

use crate::backend::{Cmd, Ev, Item, Msg, RoomSummary};

const NARROW: f32 = 720.;
const ROOM_ROW_H: f32 = 60.;

#[derive(PartialEq)]
enum Screen {
    Starting,
    Email,
    Code,
    Main,
}

pub struct BeeperApp {
    cmd: mpsc::Sender<Cmd>,
    screen: Screen,
    busy: bool,
    error: Option<SharedString>,
    email: Entity<InputState>,
    code: Entity<InputState>,
    recovery: Entity<InputState>,
    composer: Entity<InputState>,
    need_recovery: bool,
    rooms: Vec<RoomSummary>,
    open: Option<OwnedRoomId>,
    msgs: Vec<Msg>,
    more: bool,
    loading: bool,
    timeline: ListState,
    rooms_scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
    _events: Task<()>,
}

impl BeeperApp {
    pub fn new(
        cmd: mpsc::Sender<Cmd>,
        mut events: mpsc::Receiver<Ev>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("123456"));
        let recovery = cx.new(|cx| InputState::new(window, cx).placeholder("Recovery code").masked(true));
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Message"));

        let mut subs = Vec::new();
        for (input, on_enter) in [
            (&email, Self::send_code as fn(&mut Self, &mut Window, &mut Context<Self>)),
            (&code, Self::submit_code),
            (&recovery, Self::recover),
            (&composer, Self::send_message),
        ] {
            subs.push(cx.subscribe_in(input, window, move |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { shift: false, .. } => on_enter(this, window, cx),
                InputEvent::Focus => crate::show_keyboard(),
                InputEvent::Blur => crate::hide_keyboard(),
                _ => {}
            }));
        }

        let timeline = ListState::new(0, ListAlignment::Bottom, px(800.));
        let weak = cx.entity().downgrade();
        timeline.set_scroll_handler(move |ev, _, cx| {
            if ev.visible_range.start == 0 {
                let _ = weak.update(cx, |this, cx| this.load_older(cx));
            }
        });

        let events = cx.spawn(async move |this, cx| {
            while let Some(ev) = events.recv().await {
                if this.update(cx, |this, cx| this.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            cmd,
            screen: Screen::Starting,
            busy: false,
            error: None,
            email,
            code,
            recovery,
            composer,
            need_recovery: false,
            rooms: Vec::new(),
            open: None,
            msgs: Vec::new(),
            more: false,
            loading: false,
            timeline,
            rooms_scroll: UniformListScrollHandle::new(),
            _subs: subs,
            _events: events,
        }
    }

    fn send(&mut self, cmd: Cmd, cx: &mut Context<Self>) -> bool {
        let ok = self.cmd.try_send(cmd).is_ok();
        if !ok {
            self.error = Some("Still working on the previous request, try again".into());
        }
        cx.notify();
        ok
    }

    fn on_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::NeedLogin => {
                self.screen = Screen::Email;
                self.busy = false;
            }
            Ev::CodeSent => {
                self.screen = Screen::Code;
                self.busy = false;
                self.error = None;
            }
            Ev::Ready => {
                self.screen = Screen::Main;
                self.busy = false;
                self.error = None;
            }
            Ev::NeedRecovery(need) => {
                self.need_recovery = need;
                self.busy = false;
            }
            Ev::Rooms(changed) => {
                let mut by_id: HashMap<_, _> = self.rooms.drain(..).map(|r| (r.id.clone(), r)).collect();
                by_id.extend(changed.into_iter().map(|r| (r.id.clone(), r)));
                self.rooms = by_id.into_values().collect();
                self.rooms.sort_unstable_by_key(|r| std::cmp::Reverse(r.ts));
            }
            Ev::RoomGone(id) => {
                self.rooms.retain(|r| r.id != id);
                if self.open.as_ref() == Some(&id) {
                    self.close_room();
                }
            }
            Ev::History { room, items, reset, more } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                self.loading = false;
                self.more = more;
                let mut page = Vec::new();
                for item in items {
                    match item {
                        Item::Msg(m) => page.push(m),
                        Item::Edit { target, body } => {
                            apply_edit(&mut page, &target, body);
                        }
                    }
                }
                if reset {
                    self.timeline.reset(page.len());
                    self.msgs = page;
                } else {
                    self.timeline.splice(0..0, page.len());
                    page.append(&mut self.msgs);
                    self.msgs = page;
                }
            }
            Ev::Live { room, items } => {
                if self.open.as_ref() != Some(&room) || self.loading && self.msgs.is_empty() {
                    return;
                }
                for item in items {
                    match item {
                        Item::Msg(m) => {
                            // A just-fetched history page can already contain events from this sync.
                            if self.msgs.iter().rev().take(64).any(|x| x.id == m.id) {
                                continue;
                            }
                            let at = self.msgs.len();
                            self.msgs.push(m);
                            self.timeline.splice(at..at, 1);
                        }
                        Item::Edit { target, body } => {
                            if let Some(ix) = apply_edit(&mut self.msgs, &target, body) {
                                self.timeline.splice(ix..ix + 1, 1);
                            }
                        }
                    }
                }
            }
            Ev::Error(e) => {
                self.error = Some(e.into());
                self.busy = false;
                self.loading = false;
            }
        }
        cx.notify();
    }

    fn send_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let email = self.email.read(cx).value().trim().to_string();
        if email.is_empty() || self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::SendCode(email), cx);
    }

    fn submit_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        if code.is_empty() || self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::SubmitCode(code), cx);
    }

    fn recover(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let key = self.recovery.read(cx).value().trim().to_string();
        if key.is_empty() || self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::Recover(key), cx);
    }

    fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.composer.read(cx).value().trim().to_string();
        let Some(room) = self.open.clone() else { return };
        if body.is_empty() {
            return;
        }
        if self.send(Cmd::Send(room, body), cx) {
            self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        }
    }

    fn open_room(&mut self, id: OwnedRoomId, cx: &mut Context<Self>) {
        if self.open.as_ref() == Some(&id) {
            return;
        }
        self.close_room();
        self.open = Some(id.clone());
        self.loading = self.send(Cmd::Open(id), cx);
    }

    fn close_room(&mut self) {
        self.open = None;
        self.msgs.clear();
        self.timeline.reset(0);
        self.more = false;
        self.loading = false;
    }

    fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.more && !self.loading && self.open.is_some() {
            self.loading = self.send(Cmd::LoadOlder, cx);
        }
    }

    fn render_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let on_code = self.screen == Screen::Code;
        let (title, hint, input, label) = if on_code {
            ("Check your email", "Enter the code Beeper sent you.", &self.code, "Sign in")
        } else {
            ("Sign in to Beeper", "We will email you a sign-in code.", &self.email, "Send code")
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .gap_3()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(hint))
                    .child(Input::new(input))
                    .child(
                        Button::new("login-submit")
                            .primary()
                            .label(label)
                            .loading(self.busy)
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if on_code { this.submit_code(window, cx) } else { this.send_code(window, cx) }
                            })),
                    )
                    .when(on_code, |el| {
                        el.child(Button::new("login-back").ghost().label("Use a different email").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Email;
                                this.error = None;
                                cx.notify();
                            }),
                        ))
                    })
                    .children(self.error.clone().map(|e| div().text_sm().text_color(cx.theme().danger).child(e))),
            )
    }

    fn render_rooms(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let list = uniform_list(
            "rooms",
            self.rooms.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|ix| {
                        let room = &this.rooms[ix];
                        let selected = this.open.as_ref() == Some(&room.id);
                        let id = room.id.clone();
                        v_flex()
                            .id(ix)
                            .w_full()
                            .h(px(ROOM_ROW_H))
                            .px_3()
                            .justify_center()
                            .gap_0p5()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .when(selected, |el| el.bg(cx.theme().list_active))
                            .hover(|el| el.bg(cx.theme().list_hover))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.open_room(id.clone(), cx)))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(room.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(room.preview.clone().unwrap_or_default()),
                            )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.rooms_scroll)
        .size_full();

        v_flex()
            .size_full()
            .child(header(cx).child(div().font_weight(FontWeight::SEMIBOLD).child("Chats")))
            .child(if self.rooms.is_empty() {
                div().p_4().text_sm().text_color(cx.theme().muted_foreground).child("Syncing chats...").into_any_element()
            } else {
                div().flex_1().min_h_0().child(list).into_any_element()
            })
    }

    fn render_chat(&mut self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .open
            .as_ref()
            .and_then(|id| self.rooms.iter().find(|r| &r.id == id))
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let timeline = list(
            self.timeline.clone(),
            cx.processor(|this, ix: usize, _, cx| {
                let m = &this.msgs[ix];
                let bubble = v_flex()
                    .max_w(relative(0.8))
                    .px_3()
                    .py_1p5()
                    .rounded_lg()
                    .when(!m.own, |el| {
                        el.bg(cx.theme().muted).child(
                            div().text_xs().font_weight(FontWeight::MEDIUM).text_color(cx.theme().muted_foreground).child(m.sender.clone()),
                        )
                    })
                    .when(m.own, |el| el.bg(cx.theme().primary).text_color(cx.theme().primary_foreground))
                    .child(div().text_sm().child(m.body.clone()));
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .when(m.own, |el| el.justify_end())
                    .child(bubble)
                    .into_any_element()
            }),
        )
        .size_full();

        v_flex()
            .size_full()
            .child(
                header(cx)
                    .gap_2()
                    .when(narrow, |el| {
                        el.child(
                            Button::new("back").ghost().label("Back").on_click(cx.listener(|this, _, _, cx| {
                                this.close_room();
                                cx.notify();
                            })),
                        )
                    })
                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(name)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(timeline)
                    .when(self.loading && self.msgs.is_empty(), |el| {
                        el.child(div().p_4().text_sm().text_color(cx.theme().muted_foreground).child("Loading messages..."))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().min_w_0().child(Input::new(&self.composer)))
                    .child(
                        Button::new("send")
                            .primary()
                            .label("Send")
                            .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx))),
                    ),
            )
    }

    fn render_banners(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .when(self.need_recovery, |el| {
                el.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .child(div().text_sm().child(
                            "Enter your Beeper recovery code to read encrypted messages on this device.",
                        ))
                        .child(
                            h_flex().gap_2().child(div().flex_1().min_w_0().child(Input::new(&self.recovery))).child(
                                Button::new("recover")
                                    .primary()
                                    .label("Unlock")
                                    .loading(self.busy)
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| this.recover(window, cx))),
                            ),
                        ),
                )
            })
            .children(self.error.clone().map(|e| {
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().text_sm().text_color(cx.theme().danger).child(e))
                    .child(Button::new("dismiss-error").ghost().label("Dismiss").on_click(cx.listener(|this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    })))
            }))
    }
}

fn header(cx: &App) -> Div {
    h_flex().h(px(48.)).flex_none().px_3().items_center().border_b_1().border_color(cx.theme().border)
}

/// Replaces the body of `target` in `msgs`; returns its index.
fn apply_edit(msgs: &mut [Msg], target: &str, body: String) -> Option<usize> {
    let ix = msgs.iter().rposition(|m| m.id == target)?;
    msgs[ix].body = body;
    Some(ix)
}

impl Render for BeeperApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (top, bottom) = crate::insets();
        let root = v_flex()
            .size_full()
            .pt(px(top))
            .pb(px(bottom))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        match self.screen {
            Screen::Starting => root
                .items_center()
                .justify_center()
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Connecting..."))
                .into_any_element(),
            Screen::Email | Screen::Code => root.child(self.render_login(cx)).into_any_element(),
            Screen::Main => {
                let narrow = window.viewport_size().width < px(NARROW);
                let body = if narrow {
                    if self.open.is_some() {
                        self.render_chat(true, cx).into_any_element()
                    } else {
                        self.render_rooms(cx).into_any_element()
                    }
                } else {
                    h_flex()
                        .size_full()
                        .child(
                            div()
                                .w(px(300.))
                                .flex_none()
                                .h_full()
                                .border_r_1()
                                .border_color(cx.theme().border)
                                .child(self.render_rooms(cx)),
                        )
                        .child(div().flex_1().min_w_0().h_full().child(if self.open.is_some() {
                            self.render_chat(false, cx).into_any_element()
                        } else {
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Select a chat")
                                .into_any_element()
                        }))
                        .into_any_element()
                };
                root.child(self.render_banners(cx)).child(div().flex_1().min_h_0().child(body)).into_any_element()
            }
        }
    }
}
