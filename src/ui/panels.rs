//! Settings, the account's chat networks, and the login flow that adds one.

use std::collections::HashMap;

use gpui_kit::component::switch::Switch;

use super::*;
use crate::applock::{Goal, LockMode};
use crate::i18n::LanguageChoice;
use super::receipt::{IncomingStatus, ReceiptStyle};
use super::rowswipe::SwipeAction;
use crate::{ChatColour, ThemeChoice};

/// A settings row, and the widest a settings page grows.
const ROW_H: f32 = 52.;
const PANEL_W: f32 = 560.;
use crate::bridges::{self, Step, StepKind};
use crate::i18n::t;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Panel {
    Settings,
    Networks,
    /// Choosing which network to add.
    Add,
    Login,
    NewChat,
}

impl BeeperApp {
    pub(super) fn open_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Networks && self.beeper {
            self.working = self.send(Cmd::Networks, cx);
        }
        if panel == Panel::Settings {
            self.load_devices(cx);
        }
        self.panel = Some(panel);
        cx.notify();
    }

    /// One level up: a login leads back to the networks, a destination back to the chats.
    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        if self.cancel_change(cx) {
            return;
        }
        let group = self.panel == Some(Panel::NewChat) && self.new_chat_back(cx);
        self.panel = match self.panel {
            Some(Panel::Login) => {
                self.end_login(cx);
                Some(Panel::Networks)
            }
            Some(Panel::Add) => Some(Panel::Networks),
            Some(Panel::NewChat) if group => Some(Panel::NewChat),
            _ => None,
        };
        cx.notify();
    }

    fn end_login(&mut self, cx: &mut Context<Self>) {
        (self.step, self.flows, self.working) = (None, None, false);
        self.fields.clear();
        self.field_subs.clear();
        if let Some(qr) = self.qr.take() {
            qr.remove_asset(cx);
        }
    }

    fn set_theme(&mut self, theme: ThemeChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.theme = theme;
        self.settings.save(&self.dir);
        cx.set_global(theme);
        crate::sync_theme(Some(window), cx);
        cx.notify();
    }

    /// One pill per choice, under its label, as the lock timeouts are; a tap saves it.
    fn choices<T: Copy + PartialEq + 'static>(&self, id: &'static str, all: impl IntoIterator<Item = T>, current: T, label: fn(T) -> &'static str, set: fn(&mut crate::Settings, T), cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        h_flex().flex_wrap().gap(px(4.)).pb(px(8.)).children(all.into_iter().enumerate().map(|(ix, choice)| {
            let on = current == choice;
            div()
                .id((id, ix))
                .h(px(36.))
                .px(px(GAP))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(fs(14.))
                .text_color(if on { p.ground } else { p.muted })
                .bg(if on { p.text } else { p.raised })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    set(&mut this.settings, choice);
                    this.settings.save(&this.dir);
                    cx.notify();
                }))
                .child(label(choice))
        }))
    }

    fn swipe_choices(&self, left: bool, cx: &mut Context<Self>) -> Div {
        if left {
            self.choices("swipe-left", SwipeAction::ALL, self.settings.swipe_left, SwipeAction::label, |s, a| s.swipe_left = a, cx)
        } else {
            self.choices("swipe-right", SwipeAction::ALL, self.settings.swipe_right, SwipeAction::label, |s, a| s.swipe_right = a, cx)
        }
    }

    fn set_language(&mut self, language: LanguageChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.language = language;
        self.settings.save(&self.dir);
        crate::set_platform_language(language);
        let before = crate::i18n::polish();
        crate::i18n::set(language);
        if crate::i18n::polish() != before {
            self.relabel(window, cx);
        }
        cx.notify();
    }

    /// The app's language may have been changed in Android's settings while the app was away.
    pub(super) fn follow_platform_language(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::apply_language(&mut self.settings, &self.dir) {
            self.relabel(window, cx);
        }
    }

    /// Builds again what was made in the previous language: placeholders, previews, the open chat's rows.
    fn relabel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Placeholders are set once when an input is created, so they are set again here.
        let fields = [
            (&self.cookie_paste, t!("Paste cookies or a cURL command")),
            (&self.email, t!("you@example.com")),
            (&self.recovery, t!("Recovery code")),
            (&self.composer, t!("Message")),
            (&self.search, t!("Search chats")),
            (&self.find.input, t!("Search this chat")),
            (&self.emoji_pick.search, t!("Search emoji")),
        ];
        for (input, text) in fields {
            input.update(cx, |input, cx| input.set_placeholder(text, window, cx));
        }
        self.send(Cmd::Relabel, cx);
        cx.refresh_windows();
    }

    fn set_chat_colour(&mut self, colour: ChatColour, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.chat_colour = colour;
        self.settings.save(&self.dir);
        cx.set_global(colour);
        crate::sync_theme(Some(window), cx);
        cx.notify();
    }

    fn set_incognito(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.incognito = on;
        self.settings.save(&self.dir);
        self.send(Cmd::Incognito(on), cx);
    }

    fn set_incognito_keyboard(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.incognito_keyboard = on;
        self.settings.save(&self.dir);
        crate::ime::set_incognito(on);
        cx.notify();
    }

    fn set_stay_connected(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.stay_connected = on;
        self.settings.save(&self.dir);
        crate::stay_connected(on);
        cx.notify();
    }

    pub(super) fn set_lock(&mut self, mode: Option<LockMode>, timeout: Option<u32>, secure: Option<bool>, cx: &mut Context<Self>) {
        let l = &mut self.settings.lock;
        (l.mode, l.timeout, l.secure) = (mode.unwrap_or(l.mode), timeout.unwrap_or(l.timeout_secs()), secure.unwrap_or(l.secure));
        self.settings.save(&self.dir);
        let l = &self.settings.lock;
        crate::applock::configure(l.mode, l.timeout, l.secure);
        cx.notify();
    }

    fn choose_network(&mut self, bridge: &str, cx: &mut Context<Self>) {
        self.end_login(cx);
        self.panel = Some(Panel::Login);
        self.working = self.send(Cmd::Flows(bridge.to_owned()), cx);
    }

    fn start_flow(&mut self, bridge: String, flow: String, cx: &mut Context<Self>) {
        self.working = self.send(Cmd::LoginStart { bridge, flow }, cx);
    }

    fn submit(&mut self, values: HashMap<String, String>, cx: &mut Context<Self>) {
        if let Some(step) = self.step.clone() {
            self.error = None;
            self.working = self.send(Cmd::LoginSubmit(step, values), cx);
        }
    }

    fn submit_fields(&mut self, cx: &mut Context<Self>) {
        let values = self.fields.iter().map(|(id, input)| (id.clone(), input.read(cx).value().trim().to_string())).collect();
        self.submit(values, cx);
    }

    /// Hands the bridge the cookies it asked for, out of whatever the browser gave us.
    fn submit_cookies(&mut self, pasted: &str, cx: &mut Context<Self>) {
        let Some(Step { kind: StepKind::Cookies(wanted), .. }) = &self.step else { return };
        match bridges::fill_cookies(&wanted.fields, &bridges::parse_cookies(pasted)) {
            Ok(values) => self.submit(values, cx),
            Err(missing) => {
                self.error = Some(t!("The sign-in is not complete: the \"{missing}\" cookie is missing.", missing = missing).into());
                cx.notify();
            }
        }
    }

    fn browser_login(&mut self, cx: &mut Context<Self>) {
        let Some(Step { kind: StepKind::Cookies(wanted), .. }) = &self.step else { return };
        let cookies = crate::web_login(cx, wanted);
        cx.spawn(async move |this, cx| {
            let Some(cookies) = cookies.await else { return };
            let _ = this.update(cx, |this, cx| this.submit_cookies(&cookies, cx));
        })
        .detach();
    }

    pub(super) fn on_panel_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::Networks(networks) => (self.networks, self.working) = (Some(networks), false),
            Ev::Flows(bridge, flows) if self.panel == Some(Panel::Login) => match <[_; 1]>::try_from(flows) {
                // Nothing to choose from.
                Ok([only]) => self.start_flow(bridge, only.id, cx),
                Err(flows) => (self.flows, self.working) = (Some((bridge, flows)), false),
            },
            Ev::LoginStep(step) if self.panel == Some(Panel::Login) => {
                self.end_login(cx);
                match &step.kind {
                    StepKind::Show { kind, data } => {
                        if kind == "qr" {
                            self.qr = qr_image(data);
                        }
                        // The bridge answers this one when the other device has approved.
                        self.send(Cmd::LoginSubmit(step.clone(), HashMap::new()), cx);
                    }
                    StepKind::Done => {
                        self.send(Cmd::Networks, cx);
                    }
                    _ => {}
                }
                self.step = Some(step);
            }
            Ev::PanelFailed => {
                self.working = false;
                // A wait that failed cannot be resumed; the form steps can be tried again.
                if matches!(self.step, Some(Step { kind: StepKind::Show { .. }, .. })) {
                    self.end_login(cx);
                    self.panel = Some(Panel::Add);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_panel(&mut self, panel: Panel, narrow: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (title, body) = match panel {
            Panel::NewChat => return self.render_new_chat(narrow, window, cx),
            Panel::Settings if self.pad_page() => (t!("App lock"), self.render_lock_change(window, cx)),
            Panel::Settings => (t!("Settings"), self.render_settings(window, cx).into_any_element()),
            Panel::Networks => (t!("Networks"), self.render_networks(cx).into_any_element()),
            Panel::Add => (t!("Add an account"), self.render_add(cx).into_any_element()),
            Panel::Login => (t!("Sign in"), self.render_login_step(window, cx).into_any_element()),
        };
        // The destinations are reached from the navigation; only what lies under them leads back.
        let deep = matches!(panel, Panel::Add | Panel::Login) || (panel == Panel::Settings && self.pad_page());
        let edge = if narrow { GUTTER } else { GUTTER + 6. };
        v_flex()
            .size_full()
            .child(
                header()
                    .when(deep, |el| el.child(round_button("panel-back", IconName::ArrowLeft, cx).on_click(cx.listener(|this, _, _, cx| this.back(cx)))))
                    .when(!deep, |el| el.pl(px(edge)))
                    .child(screen_title(title)),
            )
            .child(
                div().id("panel-body").flex_1().min_h_0().overflow_y_scroll().when(panel == Panel::Settings && !deep, |el| el.track_scroll(&self.settings_scroll)).child(
                    v_flex().w_full().max_w(px(PANEL_W)).px(px(edge)).pt(px(8.)).pb(px(GAP * 2.)).gap(px(GAP * 2.)).child(body),
                ),
            )
            .into_any_element()
    }

    fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let theme = self.settings.theme;
        let seg = press::Segments::new("themes", window, cx);
        let chosen = [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System].iter().position(|&t| t == theme).unwrap_or(0);
        let slider = seg.pill("themes-pill", chosen, p.text, window, cx);
        let measured = slider.is_some();
        let choice = |ix: usize, label: &'static str, value: ThemeChoice, cx: &mut Context<Self>| {
            let on = theme == value;
            div()
                .id(label)
                .relative()
                .h(px(36.))
                .px(px(GAP))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(fs(14.))
                .flex_1()
                .justify_center()
                .text_color(if on { p.ground } else { p.muted })
                .when(on && !measured, |el| el.bg(p.text))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(value, window, cx)))
                .child(seg.probe(ix))
                .child(label)
        };
        let themes = h_flex()
            .p(px(4.))
            .rounded_full()
            .bg(p.raised)
            .relative()
            .child(seg.container())
            .children(slider)
            .child(choice(0, t!("Dark"), ThemeChoice::Dark, cx))
            .child(choice(1, t!("Light"), ThemeChoice::Light, cx))
            .child(choice(2, t!("System"), ThemeChoice::System, cx));
        let language = self.settings.language;
        let lang_seg = press::Segments::new("languages", window, cx);
        let lang_ix = [LanguageChoice::System, LanguageChoice::English, LanguageChoice::Polish].iter().position(|&l| l == language).unwrap_or(0);
        let lang_slider = lang_seg.pill("languages-pill", lang_ix, p.text, window, cx);
        let lang_measured = lang_slider.is_some();
        let pick = |ix: usize, label: &'static str, value: LanguageChoice, cx: &mut Context<Self>| {
            let on = language == value;
            div()
                // Not the label: "System" is also a theme option's id in the same card.
                .id(("language", ix))
                .relative()
                .h(px(36.))
                .px(px(GAP))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(fs(14.))
                .flex_1()
                .justify_center()
                .text_color(if on { p.ground } else { p.muted })
                .when(on && !lang_measured, |el| el.bg(p.text))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.set_language(value, window, cx)))
                .child(lang_seg.probe(ix))
                .child(label)
        };
        let languages = h_flex()
            .p(px(4.))
            .rounded_full()
            .bg(p.raised)
            .relative()
            .child(lang_seg.container())
            .children(lang_slider)
            .child(pick(0, t!("System"), LanguageChoice::System, cx))
            .child(pick(1, "English", LanguageChoice::English, cx))
            .child(pick(2, "Polski", LanguageChoice::Polish, cx));
        let swatches = h_flex().flex_wrap().gap_x(px(4.)).pb(px(8.)).children(ChatColour::ALL.map(|colour| {
            let on = self.settings.chat_colour == colour;
            let name = t!("Chat colour: {colour}", colour = colour.label());
            let fill = colour.fill().map_or(p.raised, |hex| rgb(hex).into());
            div()
                .id(SharedString::from(name.clone()))
                .size(px(TAP))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.set_chat_colour(colour, window, cx)))
                .child(
                    div()
                        .size(px(32.))
                        .p(px(2.))
                        .rounded_full()
                        .border_2()
                        .border_color(if on { p.text } else { p.surface })
                        .child(div().size_full().rounded_full().bg(fill)),
                )
        }));
        let weak = cx.entity().downgrade();
        let incognito = Switch::new("incognito").checked(self.settings.incognito).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_incognito(*on, cx));
        });
        let weak = cx.entity().downgrade();
        let keyboard = Switch::new("incognito-keyboard").checked(self.settings.incognito_keyboard).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_incognito_keyboard(*on, cx));
        });
        let weak = cx.entity().downgrade();
        let stay = Switch::new("stay-connected").checked(self.settings.stay_connected).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_stay_connected(*on, cx));
        });
        let user = self.user.as_ref().map(ToString::to_string).unwrap_or_default();
        let security = self.render_security(cx);
        let updates = self.render_updates(cx);
        v_flex()
            .gap(px(GAP * 2.))
            .child(section(t!("Appearance"), cx).child(
                card(cx)
                    // Under its label, as Text size is: beside it, larger text squeezes the label.
                    .child(row(t!("Theme"), "", cx))
                    .child(div().pb(px(8.)).child(themes))
                    .child(row(t!("Language"), "", cx))
                    .child(div().pb(px(8.)).child(languages))
                    .child(row(t!("Chat colour"), &t!("{colour}. Your own messages, on this device.", colour = self.settings.chat_colour.label()), cx))
                    .child(swatches)
                    .children(self.render_look(cx))
                    .children(self.render_emoji(cx))
                    .child(row(t!("Message status"), "", cx))
                    .child(self.choices("receipts", ReceiptStyle::ALL, self.settings.receipts, ReceiptStyle::label, |s, style| s.receipts = style, cx))
                    .child(row(t!("Status of received messages"), t!("Whether you have read them, and whether their sender can see that."), cx))
                    .child(self.choices("incoming", IncomingStatus::ALL, self.settings.incoming, IncomingStatus::label, |s, when| s.incoming = when, cx)),
            ))
            .child(section(t!("Chat list"), cx).child(
                card(cx)
                    .child(row(t!("Swipe left"), "", cx))
                    .child(self.swipe_choices(true, cx))
                    .child(row(t!("Swipe right"), "", cx))
                    .child(self.swipe_choices(false, cx)),
            ))
            .when(cfg!(target_os = "android"), |el| {
                el.child(section(t!("Notifications"), cx).child(card(cx).child(
                    row(t!("Sounds and importance"), t!("Direct messages, group chats and mentions each have their own."), cx).child(
                        h_flex()
                            .id("notification-settings")
                            .h(px(TAP))
                            .px(px(GAP))
                            .rounded_full()
                            .bg(Palette::of(cx).raised)
                            .cursor_pointer()
                            .on_click(|_, _, _| crate::notification_settings())
                            .child(t!("Change")),
                    ),
                ).child(
                    row(t!("Stay connected in the background"), t!("On: notifications arrive while Murmur is closed, under a quiet system notice. Off: no notice, messages arrive only while Murmur is open."), cx).child(stay),
                ).when(self.settings.stay_connected, |el| {
                    el.child(
                        row(t!("Hide the connection notice"), t!("Opens Android settings for this notice only. Messages keep arriving."), cx).child(
                            h_flex()
                                .id("connection-settings")
                                .h(px(TAP))
                                .px(px(GAP))
                                .rounded_full()
                                .bg(Palette::of(cx).raised)
                                .cursor_pointer()
                                .on_click(|_, _, _| crate::connection_notice_settings())
                                .child(t!("Change")),
                        ),
                    )
                })))
            })
            .child(section(t!("Privacy"), cx).child(card(cx).child(
                row(t!("Incognito"), t!("Others do not see that you have read their messages."), cx).child(incognito),
            ).when(cfg!(target_os = "android"), |el| {
                el.child(row(t!("Incognito keyboard"), t!("The keyboard learns nothing from what you type. Off: better suggestions and voice typing."), cx).child(keyboard))
            })))
            .child(security)
            .child(self.render_devices(cx))
            .children(updates)
            .child(
                section(t!("Account"), cx).child(
                    card(cx).child(row(if self.beeper { "Beeper" } else { "Matrix" }, &user, cx)).child(
                        h_flex()
                            .id("sign-out")
                            .min_h(px(ROW_H))
                            .text_size(fs(16.))
                            .text_color(p.negative)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                crate::background(false);
                                this.panel = None;
                                this.send(Cmd::SignOut, cx);
                            }))
                            .child(t!("Sign out")),
                    ),
                ),
            )
            .child(div().text_size(fs(12.)).text_color(p.faint).child(concat!("Murmur ", env!("CARGO_PKG_VERSION"))))
    }

    /// The app lock with its timeout, and screen security. Desktop has the app PIN only, asked at
    /// start, and no screen security.
    fn render_security(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let lock = self.settings.lock.clone();
        let android = cfg!(target_os = "android");
        let phone = android && crate::applock::available();
        let choice = |id: &'static str, label: &'static str, detail: &'static str, mode: LockMode, enabled: bool, cx: &mut Context<Self>| {
            let on = lock.mode == mode;
            row(label, detail, cx)
                .id(id)
                .when(!enabled, |el| el.text_color(p.faint))
                .when(enabled && !on, |el| el.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.change_lock(Goal::Mode(mode), cx))))
                .when(on, |el| el.child(Icon::new(IconName::Check).size_5()))
        };
        let weak = cx.entity().downgrade();
        let secure_switch = Switch::new("screen-security").checked(lock.secure).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_lock(None, None, Some(*on), cx));
        });
        let timeouts = h_flex().flex_wrap().gap(px(4.)).pb(px(8.)).children(crate::applock::TIMEOUTS.map(|(secs, label)| {
            let on = lock.timeout_secs() == secs;
            div()
                .id(SharedString::from(format!("lock-after-{secs}")))
                .h(px(36.))
                .px(px(GAP))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(fs(14.))
                .text_color(if on { p.ground } else { p.muted })
                .bg(if on { p.text } else { p.raised })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_lock(None, Some(secs), None, cx)))
                .child(crate::i18n::tr(label))
        }));
        let pin_detail = if android { t!("4 to 8 digits, set only for Murmur.") } else { t!("4 to 8 digits, asked when Murmur starts.") };
        let phone_detail =
            if phone { t!("Fingerprint, face or the phone's PIN, pattern or password.") } else { t!("Set a PIN, pattern or password in the phone's settings first.") };
        section(t!("Security"), cx).child(
            card(cx)
                .child(row(t!("App lock"), t!("Asked for when Murmur opens."), cx))
                .child(choice("lock-off", t!("Off|app lock"), "", LockMode::Off, true, cx))
                .when(android, |el| el.child(choice("lock-phone", t!("Phone lock"), phone_detail, LockMode::Phone, phone || lock.mode == LockMode::Phone, cx)))
                .child(choice("lock-pin", t!("App PIN"), pin_detail, LockMode::Pin, true, cx))
                .when(lock.mode == LockMode::Pin, |el| {
                    el.child(
                        row(t!("Change PIN"), "", cx)
                            .id("lock-change-pin")
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.change_lock(Goal::ChangePin, cx)))
                            .child(Icon::new(IconName::ChevronRight).size_5().text_color(p.faint)),
                    )
                })
                .when(android && lock.mode != LockMode::Off, |el| el.child(row(t!("Lock after leaving the app"), "", cx)).child(timeouts))
                .when(android, |el| el.child(row(t!("Screen security"), t!("Blocks screenshots and hides the app in the recent apps list."), cx).child(secure_switch)))
                .when(self.unencrypted, |el| {
                    el.child(row(
                        t!("Data on this device is not encrypted"),
                        t!("Signed in before Murmur encrypted its data. Sign out and in again to encrypt it; keep your recovery key to bring back message history."),
                        cx,
                    ))
                }),
        )
        .into_any_element()
    }

    fn render_networks(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        if !self.beeper {
            return div()
                .text_size(fs(14.))
                .line_height(fs(20.))
                .text_color(p.muted)
                .child(t!("Networks are a Beeper feature. Chats from WhatsApp, Signal and the rest come through Beeper's bridges, and this account is on another Matrix server."))
                .into_any_element();
        }
        let note = |text: &'static str| div().text_size(fs(14.)).text_color(p.faint).child(text);
        let list = match &self.networks {
            None => note(if self.working { t!("Loading…") } else { t!("Could not load the networks.") }).into_any_element(),
            Some(networks) if networks.is_empty() => note(t!("No chat network is connected yet.")).into_any_element(),
            Some(networks) => v_flex()
                .gap(px(GAP * 2.))
                .children(networks.iter().map(|network| {
                    let stopped = network.state != "RUNNING";
                    section(&network.name, cx).child(
                        card(cx)
                            .when(stopped, |el| el.child(row(&t!("Bridge {state}", state = pretty(&network.state).to_lowercase()), "", cx).text_color(p.warning)))
                            .when(network.logins.is_empty() && !stopped, |el| el.child(row(t!("Not signed in"), "", cx).text_color(p.faint)))
                            .children(network.logins.iter().map(|login| {
                                let (bridge, id) = (network.bridge.clone(), login.id.clone());
                                let ok = login.state == "CONNECTED";
                                row(&login.name, &pretty(&login.state), cx).when(!ok, |el| el.text_color(p.negative)).child(
                                    h_flex()
                                        .id(SharedString::from(format!("logout-{}-{}", network.bridge, login.id)))
                                        .h(px(TAP))
                                        .flex_none()
                                        .text_size(fs(14.))
                                        .text_color(p.negative)
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.working = this.send(Cmd::Logout { bridge: bridge.clone(), login: id.clone() }, cx);
                                        }))
                                        .child(t!("Sign out")),
                                )
                            })),
                    )
                }))
                .into_any_element(),
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(h_flex().child(Button::new("add-network").primary().h(px(TAP)).px(px(GUTTER + 4.)).label(t!("Add an account")).on_click(cx.listener(|this, _, _, cx| {
                this.open_panel(Panel::Add, cx);
            }))))
            .child(list)
            .into_any_element()
    }

    fn render_add(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        v_flex()
            .gap(px(GAP))
            .child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(
                t!("The account is added to Beeper itself, so every Beeper app you use gets it, not only this one."),
            ))
            .child(card(cx).children(bridges::KNOWN.iter().map(|&(bridge, name)| {
                h_flex()
                    .id(bridge)
                    .h(px(ROW_H))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_network(bridge, cx)))
                    .child(div().flex_1().text_size(fs(16.)).child(name))
                    .child(Icon::new(IconName::ChevronRight).size_4().text_color(p.faint))
            })))
    }

    fn render_login_step(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dim = cx.theme().muted_foreground;
        let note = |text: &str| div().text_size(fs(14.)).text_color(dim).child(text.to_owned());
        let Some(step) = self.step.clone() else {
            // Before the first step: starting the bridge, or choosing between ways to sign in.
            return match &self.flows {
                Some((bridge, flows)) => v_flex()
                    .gap_2()
                    .child(note(t!("Choose how to sign in.")))
                    .children(flows.iter().map(|flow| {
                        let (bridge, id) = (bridge.clone(), flow.id.clone());
                        v_flex()
                            .id(SharedString::from(flow.id.clone()))
                            .p_3()
                            .rounded(px(12.))
                            .bg(cx.theme().secondary)
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(cx.theme().secondary_hover)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.start_flow(bridge.clone(), id.clone(), cx)))
                            .child(div().text_size(fs(16.)).child(flow.name.clone()))
                            .child(div().text_size(fs(13.)).text_color(dim).child(flow.description.clone()))
                    }))
                    .into_any_element(),
                None => note(if self.working { t!("Starting… this can take a minute.") } else { t!("Could not start the sign-in.") }).into_any_element(),
            };
        };
        let name = bridges::display_name(&step.bridge).to_owned();
        let body = match &step.kind {
            StepKind::Input(wanted) => {
                if self.fields.is_empty() {
                    for field in wanted {
                        let state = cx.new(|cx| InputState::new(window, cx).placeholder(field.name.clone()).masked(field.kind == "password"));
                        // Codes, PINs and passwords must not be autocorrected.
                        let keyboard = match field.kind.as_str() {
                            "email" => Keyboard::Email,
                            _ => Keyboard::Key,
                        };
                        self.field_subs.push(cx.subscribe_in(&state, window, move |this, _, event: &InputEvent, _, cx| match event {
                            InputEvent::PressEnter { .. } => this.submit_fields(cx),
                            other => keyboard_follows(other, keyboard, cx),
                        }));
                        self.fields.push((field.id.clone(), state));
                    }
                }
                v_flex()
                    .gap_3()
                    .children(wanted.iter().zip(&self.fields).map(|(field, (_, input))| {
                        v_flex()
                            .gap_1()
                            .child(div().text_size(fs(14.)).font_weight(FontWeight::MEDIUM).child(field.name.clone()))
                            .child(Input::new(input))
                            .when(!field.description.is_empty(), |el| el.child(note(&field.description)))
                    }))
                    .child(h_flex().child(
                        Button::new("login-continue")
                            .primary()
                            .label(t!("Continue"))
                            .loading(self.working)
                            .disabled(self.working)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_fields(cx))),
                    ))
                    .into_any_element()
            }
            StepKind::Cookies(_) => {
                let phone = cfg!(target_os = "android");
                v_flex()
                    .gap_3()
                    .child(note(if phone {
                        t!("A browser opens for you to sign in. It closes by itself when you are in.")
                    } else {
                        t!("Sign in on the page that opens in your browser. Then copy the request's cookies from the browser's developer tools (Network, any request to the site, Copy as cURL) and paste them here.")
                    }))
                    .child(h_flex().child(
                        Button::new("login-browser")
                            .primary()
                            .label(if phone { t!("Sign in") } else { t!("Open the sign-in page") })
                            .disabled(self.working)
                            .on_click(cx.listener(|this, _, _, cx| this.browser_login(cx))),
                    ))
                    .child(Input::new(&self.cookie_paste))
                    .child(h_flex().child(
                        Button::new("login-cookies")
                            .label(t!("Continue with pasted cookies"))
                            .loading(self.working)
                            .disabled(self.working)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let pasted = this.cookie_paste.read(cx).value().to_string();
                                this.submit_cookies(&pasted, cx);
                            })),
                    ))
                    .into_any_element()
            }
            StepKind::Show { kind, data } => v_flex()
                .gap_3()
                .items_start()
                .map(|el| match (&self.qr, kind.as_str()) {
                    (Some(qr), _) => el.child(img(qr.clone()).size(px(240.)).rounded(px(12.))),
                    (None, "nothing") => el,
                    (None, _) => el.child(div().text_size(fs(28.)).font_weight(FontWeight::SEMIBOLD).child(data.clone())),
                })
                .child(note(t!("Waiting for the other device…")))
                .into_any_element(),
            StepKind::Done => v_flex()
                .gap_3()
                .child(div().text_size(fs(16.)).child(t!("{name} is connected.", name = name)))
                .child(note(t!("Its chats appear in the list as the bridge brings them in.")))
                .child(h_flex().child(Button::new("login-done").primary().label(t!("Done")).on_click(cx.listener(|this, _, _, cx| this.back(cx)))))
                .into_any_element(),
        };
        v_flex()
            .gap_4()
            .child(div().text_size(fs(20.)).font_weight(FontWeight::SEMIBOLD).child(name))
            .when(!step.instructions.is_empty(), |el| el.child(div().text_size(fs(15.)).child(step.instructions.clone())))
            .child(body)
            .into_any_element()
    }
}

/// "BAD_CREDENTIALS" as "Bad credentials". The known states are mautrix bridgev2's
/// (`bridgev2/status/bridgestate.go`); a new one shows as its code in words.
fn pretty(state: &str) -> String {
    let known = match state {
        "STARTING" => t!("Starting"),
        "UNCONFIGURED" => t!("Unconfigured"),
        "RUNNING" => t!("Running"),
        "BRIDGE_UNREACHABLE" => t!("Bridge unreachable"),
        "CONNECTING" => t!("Connecting"),
        "BACKFILLING" => t!("Backfilling"),
        "CONNECTED" => t!("Connected"),
        "TRANSIENT_DISCONNECT" => t!("Transient disconnect"),
        "BAD_CREDENTIALS" => t!("Bad credentials"),
        "UNKNOWN_ERROR" => t!("Unknown error"),
        "LOGGED_OUT" => t!("Logged out"),
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    let lower = state.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// A titled group of settings.
pub(super) fn section(title: &str, cx: &App) -> Div {
    let p = Palette::of(cx);
    v_flex().gap(px(8.)).child(div().px(px(GAP)).text_size(fs(12.)).text_color(p.faint).child(title.to_owned()))
}

/// The rows of one group, on one surface.
pub(super) fn card(cx: &App) -> Div {
    v_flex().px(px(GAP)).py(px(4.)).rounded(px(RADIUS)).bg(Palette::of(cx).surface)
}

/// A setting: what it is, a line about it, and its control after it.
pub(super) fn row(label: &str, detail: &str, cx: &App) -> Div {
    let p = Palette::of(cx);
    h_flex().min_h(px(ROW_H)).py(px(8.)).gap(px(GAP)).child(
        v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_size(fs(16.)).line_height(fs(22.)).child(label.to_owned()))
            .when(!detail.is_empty(), |el| el.child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(detail.to_owned()))),
    )
}

/// The code as a picture: 8 pixels a module, with the 4-module margin scanners expect.
fn qr_image(data: &str) -> Option<Arc<Image>> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let (modules, scale, margin) = (code.width() as u32, 8, 4);
    let dark = code.to_colors();
    let picture = image::GrayImage::from_fn((modules + 2 * margin) * scale, (modules + 2 * margin) * scale, |x, y| {
        let (col, row) = ((x / scale).wrapping_sub(margin), (y / scale).wrapping_sub(margin));
        let on = col < modules && row < modules && dark[(row * modules + col) as usize] == qrcode::Color::Dark;
        image::Luma([if on { 0 } else { 255 }])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(Arc::new(Image::from_bytes(ImageFormat::Png, png.into_inner())))
}

/// The Emoji setting: a colour emoji font of the user's own, or the system's.
#[derive(Default)]
pub(super) struct EmojiChoice {
    /// A font was chosen or dropped since start: fonts cannot be swapped live.
    pending: bool,
    open: bool,
    copying: bool,
    error: Option<String>,
}

impl BeeperApp {
    fn render_emoji(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !crate::emoji::SUPPORTED {
            return None;
        }
        let p = Palette::of(cx);
        let chosen = self.settings.emoji_font.as_deref();
        let name = chosen.unwrap_or(t!("System|emoji"));
        let detail = match () {
            _ if self.emoji.copying => t!("Copying the font…").to_owned(),
            _ if self.emoji.pending => t!("{name}. Applies after restart.", name = name),
            _ => name.to_owned(),
        };
        let change = h_flex()
            .id("emoji-change")
            .h(px(TAP))
            .px(px(GAP))
            .rounded_full()
            .bg(p.raised)
            .when(!self.emoji.copying, |el| {
                el.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                    this.emoji.open = !this.emoji.open;
                    cx.notify();
                }))
            })
            .child(t!("Change"));
        let option = |id: &'static str, label: &'static str| h_flex().id(id).min_h(px(ROW_H)).text_size(fs(16.)).cursor_pointer().child(label);
        Some(
            v_flex()
                .child(row("Emoji", &detail, cx).child(change))
                .when_some(self.emoji.error.clone(), |el, error| el.child(div().pb(px(8.)).text_size(fs(13.)).text_color(p.negative).child(error)))
                .when(self.emoji.open, |el| {
                    el.child(option("emoji-file", t!("Choose font file…")).on_click(cx.listener(|this, _, _, cx| this.choose_emoji_font(cx))))
                        .when(chosen.is_some(), |el| {
                            el.child(option("emoji-system", t!("Use system emoji")).on_click(cx.listener(|this, _, _, cx| this.use_system_emoji(cx))))
                        })
                }),
        )
    }

    fn choose_emoji_font(&mut self, cx: &mut Context<Self>) {
        self.emoji.open = false;
        let picked = crate::pick_file(cx);
        let dir = self.dir.clone();
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            // One import at a time: they share the file being written.
            let started = this.update(cx, |this, cx| {
                let free = !std::mem::replace(&mut this.emoji.copying, true);
                cx.notify();
                free
            });
            if !matches!(started, Ok(true)) {
                return;
            }
            let done = cx.background_spawn(async move { crate::emoji::import(&path, &dir) }).await;
            let _ = this.update(cx, |this, cx| {
                this.emoji.copying = false;
                match done {
                    Ok(family) => {
                        this.settings.emoji_font = Some(family);
                        this.settings.save(&this.dir);
                        this.emoji.pending = true;
                        this.emoji.error = None;
                    }
                    Err(e) => this.emoji.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn use_system_emoji(&mut self, cx: &mut Context<Self>) {
        crate::emoji::remove(&self.dir);
        self.settings.emoji_font = None;
        self.settings.save(&self.dir);
        self.emoji.pending = true;
        self.emoji.open = false;
        self.emoji.error = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{pretty, qr_image};

    #[test]
    fn qr_code_is_a_png_with_its_margin() {
        let image = qr_image("2@abcdefghijklmnopqrstuvwxyz,0123456789").unwrap();
        let decoded = image::load_from_memory(image.bytes()).unwrap().to_luma8();
        assert_eq!(decoded.width(), decoded.height());
        // White margin, then the finder pattern's dark corner.
        assert_eq!((decoded.get_pixel(0, 0).0, decoded.get_pixel(4 * 8, 4 * 8).0), ([255], [0]));
        assert_eq!(pretty("BAD_CREDENTIALS"), "Bad credentials");
    }
}
