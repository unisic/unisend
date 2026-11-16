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

/// Discord's bridge on Beeper, the one with servers to switch on.
const DISCORD: &str = "discordgo";
use crate::i18n::t;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Panel {
    Settings,
    Networks,
    /// Choosing which network to add.
    Add,
    /// One signed-in account of a network: its state, signing in again, signing out.
    Account,
    Login,
    NewChat,
}

/// A page under Settings: a category of its list, or the choices of one setting in a category.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Page {
    Appearance,
    Chats,
    Notifications,
    Privacy,
    Devices,
    Account,
    Updates,
    Emoji,
    Receipts,
    SwipeLeft,
    SwipeRight,
    Lock,
    LockAfter,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Appearance => t!("Appearance"),
            Self::Chats => t!("Chats"),
            Self::Notifications => t!("Notifications"),
            Self::Privacy => t!("Privacy and security"),
            Self::Devices => t!("Devices"),
            Self::Account => t!("Account"),
            Self::Updates => t!("Updates"),
            Self::Emoji => "Emoji",
            Self::Receipts => t!("Message status"),
            Self::SwipeLeft => t!("Swipe left"),
            Self::SwipeRight => t!("Swipe right"),
            Self::Lock => t!("App lock"),
            Self::LockAfter => t!("Auto-lock"),
        }
    }

    fn id(self) -> ElementId {
        ("settings-page", self as usize).into()
    }
}

impl BeeperApp {
    pub(super) fn open_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel != Panel::Login {
            self.cancel_login(cx);
        }
        if panel == Panel::Networks && self.beeper {
            self.working = self.send(Cmd::Networks, cx);
        }
        if panel == Panel::Settings {
            self.settings_pages.clear();
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
        if self.panel == Some(Panel::Settings) && self.settings_pages.pop().is_some() {
            self.page_scroll.set_offset(Point::default());
            cx.notify();
            return;
        }
        let group = self.panel == Some(Panel::NewChat) && self.new_chat_back(cx);
        self.panel = match self.panel {
            Some(Panel::Login) => {
                self.cancel_login(cx);
                self.end_login(cx);
                Some(Panel::Networks)
            }
            Some(Panel::Add | Panel::Account) => Some(Panel::Networks),
            Some(Panel::NewChat) if group => Some(Panel::NewChat),
            _ => None,
        };
        cx.notify();
    }

    /// Leaving a login before it completed tells the bridge, which would otherwise wait for it
    /// half an hour, and drops the request waiting on it.
    pub(super) fn cancel_login(&mut self, cx: &mut Context<Self>) {
        if let Some(step) = self.step.take_if(|s| s.kind != StepKind::Done) {
            self.send(Cmd::LoginCancel(step), cx);
            self.end_login(cx);
        }
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

    /// `relogin` signs that login in again in place of adding one.
    fn choose_network(&mut self, bridge: &str, relogin: Option<String>, cx: &mut Context<Self>) {
        self.cancel_login(cx);
        self.end_login(cx);
        self.relogin = relogin;
        self.panel = Some(Panel::Login);
        self.working = self.send(Cmd::Flows(bridge.to_owned()), cx);
    }

    fn start_flow(&mut self, bridge: String, flow: String, cx: &mut Context<Self>) {
        self.working = self.send(Cmd::LoginStart { bridge, flow, relogin: self.relogin.clone() }, cx);
    }

    /// Shown switched at once; the answer, or a failure, sets the list from the bridge.
    pub(super) fn set_guild(&mut self, bridge: String, login: String, guild: String, on: bool, cx: &mut Context<Self>) {
        if let Some(g) = self.guilds.as_mut().filter(|(l, _)| *l == login).and_then(|(_, list)| list.iter_mut().find(|g| g.id == guild)) {
            g.bridged = on;
        }
        self.send(Cmd::BridgeGuild { bridge, login, guild, on }, cx);
        cx.notify();
    }

    fn open_account(&mut self, bridge: String, login: String, cx: &mut Context<Self>) {
        self.guilds = None;
        if bridge == DISCORD {
            self.send(Cmd::Guilds { bridge: bridge.clone(), login: login.clone() }, cx);
        }
        self.account = Some((bridge, login));
        self.panel = Some(Panel::Account);
        cx.notify();
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
            Ev::Networks(networks) => {
                self.attention = networks
                    .iter()
                    .flat_map(|n| n.logins.iter().filter(|l| bridges::needs_sign_in(&l.state)).map(|l| (n.bridge.clone(), l.name.clone())))
                    .collect();
                (self.networks, self.working) = (Some(networks), false);
            }
            Ev::Attention(logins) => self.attention = logins,
            Ev::Guilds(login, list) => {
                if self.account.as_ref().is_some_and(|(_, l)| *l == login) {
                    self.guilds = Some((login, list));
                }
            }
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
                // A switched server the bridge refused shows as it is again.
                if let Some((bridge, login)) = self.account.clone().filter(|(b, _)| b == DISCORD && self.panel == Some(Panel::Account)) {
                    self.send(Cmd::Guilds { bridge, login }, cx);
                }
                // A wait that failed cannot be resumed; the form steps can be tried again.
                if matches!(self.step, Some(Step { kind: StepKind::Show { .. }, .. })) {
                    self.end_login(cx);
                    self.panel = Some(Panel::Add);
                }
            }
            _ => {}
        }
    }

    /// `banners` go under the title, kept to the content's column.
    pub(super) fn render_panel(&mut self, panel: Panel, narrow: bool, banners: Option<AnyElement>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (title, body) = match panel {
            Panel::NewChat => return self.render_new_chat(narrow, window, cx),
            Panel::Settings if self.pad_page() => (t!("App lock"), self.render_lock_change(window, cx)),
            Panel::Settings if !narrow => return self.render_settings_split(banners, window, cx),
            Panel::Settings => match self.settings_pages.last().copied() {
                None => (t!("Settings"), self.render_settings(false, cx).into_any_element()),
                Some(page) => (page.title(), self.render_page(page, window, cx)),
            },
            Panel::Networks => (t!("Networks"), self.render_networks(cx).into_any_element()),
            Panel::Add => (t!("Add an account"), self.render_add(cx).into_any_element()),
            Panel::Account => (t!("Account"), self.render_network_account(cx)),
            Panel::Login => (t!("Sign in"), self.render_login_step(window, cx).into_any_element()),
        };
        // The destinations are reached from the navigation; only what lies under them leads back.
        let paged = panel == Panel::Settings && !self.settings_pages.is_empty();
        let deep = matches!(panel, Panel::Add | Panel::Account | Panel::Login) || (panel == Panel::Settings && self.pad_page()) || paged;
        let edge = if narrow { GUTTER } else { GUTTER + 6. };
        // Always the rightmost pane, under the window buttons.
        let end = frame::reserve(window);
        // A long translation ends in an ellipsis rather than past the screen edge.
        let title = screen_title(title).min_w_0().truncate();
        let title = if end > 0. { frame::drag(div().flex_1().min_w_0().h_full().flex().items_center().child(title), window, cx) } else { title };
        let settings = panel == Panel::Settings && !deep;
        let page = paged && !self.pad_page();
        let scrolled = div()
            .id("panel-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when(settings, |el| el.track_scroll(&self.settings_scroll))
            .when(page, |el| el.track_scroll(&self.page_scroll))
            .child(v_flex().w_full().max_w(px(PANEL_W)).px(px(edge)).pt(px(8.)).pb(px(GAP * 2.)).gap(px(GAP * 2.)).child(body));
        let handle = if settings { Some(&self.settings_scroll) } else if page { Some(&self.page_scroll) } else { None };
        let scrolled = match handle.filter(|_| !gpui_kit::is_mobile()) {
            Some(handle) => div().flex_1().min_h_0().relative().child(scrolled.size_full()).vertical_scrollbar(handle).into_any_element(),
            None => scrolled.into_any_element(),
        };
        v_flex()
            .size_full()
            .child(
                header()
                    .when(deep, |el| el.child(round_button("panel-back", IconName::ArrowLeft, cx).on_click(cx.listener(|this, _, _, cx| this.back(cx)))))
                    .when(!deep, |el| el.pl(px(edge)))
                    .when(end > 0., |el| el.pr(px(GUTTER - 10. + end)))
                    .child(title),
            )
            // A card standing above the cards keeps their spacing.
            .children(banners.map(|banners| {
                let card = self.need_recovery || self.error.is_some();
                div().w_full().max_w(px(PANEL_W)).px(px(edge - GUTTER)).when(card, |el| el.pb(px(GAP * 2. - 8.))).child(banners)
            }))
            .child(scrolled)
            .into_any_element()
    }

    /// The settings list: one row per category, each with what is set in it now.
    /// Settings on a wide window, laid out as the chats are: the categories where the chat list
    /// stands, the chosen one beside them in a centred column.
    fn render_settings_split(&mut self, banners: Option<AnyElement>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let edge = GUTTER + 6.;
        let end = frame::reserve(window);
        let mobile = gpui_kit::is_mobile();
        let list = div()
            .id("settings-list")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.settings_scroll)
            .child(v_flex().px(px(GUTTER)).pt(px(8.)).pb(px(GAP * 2.)).child(self.render_settings(true, cx)));
        let list = v_flex()
            .w(px(LIST_W))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(p.rule)
            .child(
                h_flex().flex_none().h(px(BAR_H)).pl(px(GUTTER)).pr(px(GUTTER - 10.)).child(frame::drag(
                    div().flex_1().when(!mobile, |el| el.h_full().flex().items_center()).child(screen_title(t!("Settings"))),
                    window,
                    cx,
                )),
            )
            .children(banners)
            .child(div().flex_1().min_h_0().relative().child(list).when(!mobile, |el| el.vertical_scrollbar(&self.settings_scroll)));
        let Some(page) = self.settings_pages.last().copied() else {
            let strip = frame::drag(div().absolute().top_0().left_0().w_full().h(px(BAR_H)), window, cx);
            let empty = v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .relative()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .text_size(fs(14.))
                .text_color(p.faint)
                .child(strip)
                .when(!mobile, |el| el.child(Icon::new(IconName::Settings).size_8()))
                .child(t!("Choose a setting"));
            return h_flex().size_full().child(list).child(empty).into_any_element();
        };
        let deep = self.settings_pages.len() > 1;
        let body = self.render_page(page, window, cx);
        let title = screen_title(page.title()).min_w_0().truncate();
        // Even room at both ends keeps the title over the column while the window buttons take the right.
        let header = h_flex().h(px(BAR_H)).flex_none().justify_center().px(px(end)).child(
            h_flex()
                .w_full()
                .max_w(px(PANEL_W))
                .h_full()
                .items_center()
                .gap(px(4.))
                .px(px(edge - 10.))
                .when(deep, |el| el.child(round_button("panel-back", IconName::ArrowLeft, cx).on_click(cx.listener(|this, _, _, cx| this.back(cx)))))
                .when(!deep, |el| el.pl(px(edge)))
                .child(frame::drag(div().flex_1().min_w_0().h_full().flex().items_center().child(title), window, cx)),
        );
        let scrolled = div()
            .id("panel-body")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.page_scroll)
            .child(v_flex().items_center().child(v_flex().w_full().max_w(px(PANEL_W)).px(px(edge)).pt(px(8.)).pb(px(GAP * 2.)).gap(px(GAP * 2.)).child(body)));
        let pane = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(div().flex_1().min_h_0().relative().child(scrolled).when(!mobile, |el| el.vertical_scrollbar(&self.page_scroll)));
        h_flex().size_full().child(list).child(pane).into_any_element()
    }

    /// `split`: beside the chosen category on a wide window, which a category row then replaces.
    fn render_settings(&mut self, split: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let s = &self.settings;
        let theme = match s.theme {
            ThemeChoice::Dark => t!("Dark theme"),
            ThemeChoice::Light => t!("Light theme"),
            ThemeChoice::System => t!("System theme"),
        };
        let look = t!("{theme}, text {size}%", theme = theme, size = s.text_size.percent());
        let chats = t!("Message status: {style}", style = s.receipts.label());
        let notices = if !cfg!(target_os = "android") {
            t!("Send a test notification")
        } else if s.stay_connected {
            t!("Arrive while Unisend is closed")
        } else {
            t!("Only while Unisend is open")
        };
        let privacy = match s.lock.mode {
            _ if self.unencrypted => t!("Data on this device is not encrypted"),
            LockMode::Off => t!("App lock off"),
            LockMode::Phone => t!("Unlocked with the phone lock"),
            LockMode::Pin => t!("Unlocked with the app PIN"),
        };
        let user = self.user.as_ref().map(ToString::to_string).unwrap_or_default();
        let updates = self.updates_summary();
        let selected = self.settings_pages.first().copied().filter(|_| split);
        let category = |page: Page, icon: IconName, summary: SharedString, cx: &mut Context<Self>| {
            link(page.id(), page.title(), summary, Some(icon), Some(IconName::ChevronRight), cx)
                // A step above the hover's fill, so the chosen page reads apart from the one under the pointer.
                .when(selected == Some(page), |el| el.bg(p.lifted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if split {
                        this.settings_pages.clear();
                    }
                    this.open_page(page, cx)
                }))
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                card(cx)
                    .child(category(Page::Appearance, IconName::Palette, look.into(), cx))
                    .child(category(Page::Chats, IconName::MessageCircle, chats.into(), cx))
                    .child(category(Page::Notifications, IconName::Bell, notices.into(), cx)),
            )
            .child(
                card(cx)
                    .child(category(Page::Privacy, IconName::Shield, privacy.into(), cx))
                    .child(category(Page::Devices, IconName::MonitorSmartphone, self.devices_summary(), cx)),
            )
            .child(
                card(cx)
                    .child(category(Page::Account, IconName::User, user.into(), cx))
                    .when_some(updates, |el, summary| el.child(category(Page::Updates, IconName::Download, summary.into(), cx))),
            )
            .child(
                card(cx).child(
                    h_flex()
                        .min_h(px(ROW_H))
                        .py(px(8.))
                        .gap(px(GAP))
                        .child(Icon::new(IconName::Info).size_5().flex_none().text_color(p.brand))
                        .child(
                            v_flex()
                                .child(div().text_size(fs(16.)).line_height(fs(22.)).child("Unisend"))
                                .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(t!("Version {version}", version = env!("CARGO_PKG_VERSION")))),
                        ),
                ),
            )
    }

    fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        // Two levels at most, and a second tap that lands before the page is drawn opens nothing.
        if self.settings_pages.len() < 2 && self.settings_pages.last() != Some(&page) {
            self.settings_pages.push(page);
            self.page_scroll.set_offset(Point::default());
        }
        cx.notify();
    }

    fn render_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.settings;
        match page {
            Page::Appearance => self.render_appearance(window, cx),
            Page::Chats => self.render_chats(window, cx),
            Page::Notifications => self.render_notifications(cx),
            Page::Privacy => self.render_privacy(cx),
            Page::Devices => self.render_devices(cx).into_any_element(),
            Page::Account => self.render_account(cx),
            Page::Updates => self.render_updates(cx).map_or_else(|| div().into_any_element(), IntoElement::into_any_element),
            Page::Emoji => self.render_emoji(cx),
            Page::Receipts => pick("receipts", ReceiptStyle::ALL, s.receipts, ReceiptStyle::label, |s, style| s.receipts = style, cx),
            Page::SwipeLeft => pick("swipe-left", SwipeAction::ALL, s.swipe_left, SwipeAction::label, |s, a| s.swipe_left = a, cx),
            Page::SwipeRight => pick("swipe-right", SwipeAction::ALL, s.swipe_right, SwipeAction::label, |s, a| s.swipe_right = a, cx),
            Page::Lock => self.render_lock_modes(cx),
            Page::LockAfter => {
                let after = s.lock.timeout_secs();
                card(cx)
                    .children(crate::applock::TIMEOUTS.map(|(secs, label)| {
                        let on = after == secs;
                        option(SharedString::from(format!("lock-after-{secs}")), crate::i18n::tr(label), "", on, cx)
                            .when(!on, |el| el.on_click(cx.listener(move |this, _, _, cx| this.set_lock(None, Some(secs), None, cx))))
                    }))
                    .into_any_element()
            }
        }
    }

    /// Two or three short choices side by side, the chosen one under a pill that slides to it.
    fn segmented<T: Copy + PartialEq + 'static>(
        id: &'static str,
        pill: &'static str,
        options: &[(T, &'static str)],
        current: T,
        set: fn(&mut Self, T, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = Palette::of(cx);
        let seg = press::Segments::new(id, window, cx);
        let chosen = options.iter().position(|&(value, _)| value == current).unwrap_or(0);
        let slider = seg.pill(pill, chosen, p.text, window, cx);
        let measured = slider.is_some();
        h_flex().p(px(4.)).rounded_full().bg(p.raised).relative().child(seg.container()).children(slider).children(options.iter().enumerate().map(|(ix, &(value, label))| {
            let on = ix == chosen;
            div()
                .id((id, ix))
                .relative()
                .h(px(36.))
                .px(px(GAP))
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(fs(14.))
                .text_color(if on { p.ground } else { p.muted })
                .when(on && !measured, |el| el.bg(p.text))
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_toggled(on.into())
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| set(this, value, window, cx)))
                .child(seg.probe(ix))
                .child(label)
        }))
    }

    fn render_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let themes = [(ThemeChoice::Dark, t!("Dark")), (ThemeChoice::Light, t!("Light")), (ThemeChoice::System, t!("System"))];
        let themes = Self::segmented("themes", "themes-pill", &themes, self.settings.theme, Self::set_theme, window, cx);
        let languages = [(LanguageChoice::System, t!("System")), (LanguageChoice::English, "English"), (LanguageChoice::Polish, "Polski")];
        let languages = Self::segmented("languages", "languages-pill", &languages, self.settings.language, Self::set_language, window, cx);
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
                .role(Role::RadioButton)
                .aria_label(name)
                .aria_toggled(on.into())
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
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                card(cx)
                    // Under their labels: beside them, larger text squeezes the label.
                    .child(row(t!("Theme"), "", cx))
                    .child(div().pb(px(8.)).child(themes))
                    .child(row(t!("Language"), "", cx))
                    .child(div().pb(px(8.)).child(languages)),
            )
            .child(card(cx).child(row(t!("Chat colour"), &t!("{colour}. Your own messages, on this device.", colour = self.settings.chat_colour.label()), cx)).child(swatches))
            .child(card(cx).children(self.render_look(cx)))
            .when(crate::emoji::SUPPORTED, |el| {
                el.child(card(cx).child(
                    link("emoji", "Emoji", self.emoji_value().into(), None, Some(IconName::ChevronRight), cx).on_click(cx.listener(|this, _, _, cx| this.open_page(Page::Emoji, cx))),
                ))
            })
            .into_any_element()
    }

    fn render_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.settings;
        let (receipts, left, right) = (s.receipts.label(), s.swipe_left.label(), s.swipe_right.label());
        let incoming = IncomingStatus::ALL.map(|when| (when, when.label()));
        let incoming = Self::segmented("incoming", "incoming-pill", &incoming, s.incoming, |this, when, _, cx| {
            this.settings.incoming = when;
            this.settings.save(&this.dir);
            cx.notify();
        }, window, cx);
        let choice = |page: Page, value: &'static str, cx: &mut Context<Self>| {
            link(page.id(), page.title(), value.into(), None, Some(IconName::ChevronRight), cx).on_click(cx.listener(move |this, _, _, cx| this.open_page(page, cx)))
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                card(cx)
                    .child(choice(Page::Receipts, receipts, cx))
                    .child(row(t!("Status of received messages"), t!("What their sender sees: sent, delivered or read."), cx))
                    .child(div().pb(px(8.)).child(incoming)),
            )
            .child(section(t!("Chat list"), cx).child(card(cx).child(choice(Page::SwipeLeft, left, cx)).child(choice(Page::SwipeRight, right, cx))))
            .into_any_element()
    }

    fn render_notifications(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let stay = Switch::new("stay-connected").checked(self.settings.stay_connected).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_stay_connected(*on, cx));
        });
        let android = |id: &'static str, label: &'static str, detail: &'static str, open: fn(), cx: &mut Context<Self>| {
            link(id, label, detail.into(), None, Some(IconName::ExternalLink), cx).on_click(move |_, _, _| open())
        };
        let test = link("test-notice", t!("Send a test notification"), t!("As a message brings one. Its buttons send nothing.").into(), None, Some(IconName::Bell), cx)
            .on_click(cx.listener(|this, _, _, cx| {
                this.send(Cmd::TestNotice, cx);
            }));
        let page = v_flex().gap(px(GAP * 2.)).child(card(cx).child(test));
        // Sounds and the connection are Android's; desktop has the system's own settings.
        if !cfg!(target_os = "android") {
            return page.into_any_element();
        }
        page.child(card(cx).child(android(
                "notification-settings",
                t!("Sounds and importance"),
                t!("Direct messages, group chats and mentions each have their own."),
                crate::notification_settings,
                cx,
            )))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(card(cx).child(row(t!("Stay connected in the background"), "", cx).child(stay)))
                    .child(footnote(t!("On: notifications arrive while Unisend is closed, under a quiet system notice. Off: no notice, messages arrive only while Unisend is open."), cx)),
            )
            .when(self.settings.stay_connected, |el| {
                el.child(card(cx).child(android(
                    "connection-settings",
                    t!("Hide the connection notice"),
                    t!("Opens Android settings for this notice only. Messages keep arriving."),
                    crate::connection_notice_settings,
                    cx,
                )))
            })
            .into_any_element()
    }

    /// Incognito and the keyboard, then the app lock and screen security. Desktop has the app PIN
    /// only, asked at start, and no screen security.
    fn render_privacy(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let android = cfg!(target_os = "android");
        let lock = self.settings.lock.clone();
        let weak = cx.entity().downgrade();
        let incognito = Switch::new("incognito").checked(self.settings.incognito).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_incognito(*on, cx));
        });
        let weak = cx.entity().downgrade();
        let keyboard = Switch::new("incognito-keyboard").checked(self.settings.incognito_keyboard).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_incognito_keyboard(*on, cx));
        });
        let weak = cx.entity().downgrade();
        let secure = Switch::new("screen-security").checked(lock.secure).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_lock(None, None, Some(*on), cx));
        });
        let after = crate::applock::TIMEOUTS.iter().find(|&&(secs, _)| secs == lock.timeout_secs()).map_or("", |&(_, label)| crate::i18n::tr(label));
        let choice = |page: Page, value: &'static str, cx: &mut Context<Self>| {
            link(page.id(), page.title(), value.into(), None, Some(IconName::ChevronRight), cx).on_click(cx.listener(move |this, _, _, cx| this.open_page(page, cx)))
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                card(cx)
                    .child(row(t!("Incognito"), t!("Others do not see that you have read their messages."), cx).child(incognito))
                    .when(android, |el| el.child(row(t!("Incognito keyboard"), t!("The keyboard learns nothing from what you type. Off: better suggestions and voice typing."), cx).child(keyboard))),
            )
            .child(
                section(t!("Security"), cx).child(
                    card(cx)
                        .child(choice(Page::Lock, lock_label(lock.mode), cx))
                        .when(lock.mode == LockMode::Pin, |el| {
                            el.child(
                                link("lock-change-pin", t!("Change PIN"), SharedString::default(), None, Some(IconName::ChevronRight), cx)
                                    .on_click(cx.listener(|this, _, _, cx| this.change_lock(Goal::ChangePin, cx))),
                            )
                        })
                        .when(android && lock.mode != LockMode::Off, |el| el.child(choice(Page::LockAfter, after, cx)))
                        .when(android, |el| el.child(row(t!("Screen security"), t!("Blocks screenshots and hides the app in the recent apps list."), cx).child(secure))),
                ),
            )
            .when(self.unencrypted, |el| {
                el.child(
                    v_flex()
                        .gap(px(8.))
                        .child(card(cx).child(row(t!("Data on this device is not encrypted"), "", cx).text_color(p.warning)))
                        .child(footnote(t!("Signed in before Unisend encrypted its data. Sign out and in again to encrypt it; keep your recovery key to bring back message history."), cx)),
                )
            })
            .into_any_element()
    }

    fn render_lock_modes(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.settings.lock.mode;
        let android = cfg!(target_os = "android");
        let phone = android && crate::applock::available();
        let choice = |id: &'static str, value: LockMode, detail: &'static str, enabled: bool, cx: &mut Context<Self>| {
            let on = mode == value;
            option(id, lock_label(value), detail, on, cx)
                .when(!enabled, |el| el.text_color(Palette::of(cx).faint))
                .when(enabled && !on, |el| el.on_click(cx.listener(move |this, _, _, cx| this.change_lock(Goal::Mode(value), cx))))
        };
        let pin_detail = if android { t!("4 to 8 digits, set only for Unisend.") } else { t!("4 to 8 digits, asked when Unisend starts.") };
        let phone_detail =
            if phone { t!("Fingerprint, face or the phone's PIN, pattern or password.") } else { t!("Set a PIN, pattern or password in the phone's settings first.") };
        v_flex()
            .gap(px(8.))
            .child(
                card(cx)
                    .child(choice("lock-off", LockMode::Off, "", true, cx))
                    .when(android, |el| el.child(choice("lock-phone", LockMode::Phone, phone_detail, phone || mode == LockMode::Phone, cx)))
                    .child(choice("lock-pin", LockMode::Pin, pin_detail, true, cx)),
            )
            .child(footnote(t!("Asked for when Unisend opens."), cx))
            .into_any_element()
    }

    fn render_account(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let user = self.user.as_ref().map(ToString::to_string).unwrap_or_default();
        v_flex()
            .gap(px(GAP * 2.))
            .child(card(cx).child(row(if self.beeper { "Beeper" } else { "Matrix" }, &user, cx)))
            .child(card(cx).child(
                h_flex()
                    .id("sign-out")
                    .min_h(px(ROW_H))
                    .text_size(fs(16.))
                    .text_color(p.negative)
                    .role(Role::Button)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        crate::background(false);
                        this.panel = None;
                        this.send(Cmd::SignOut, cx);
                    }))
                    .child(t!("Sign out")),
            ))
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
                    let bridge = network.bridge.clone();
                    section(&network.name, cx).child(
                        card(cx)
                            .when(stopped, |el| el.child(row(&plain(&network.state), "", cx).text_color(p.warning)))
                            .when(network.logins.is_empty() && !stopped, |el| {
                                el.child(row(t!("Not signed in"), "", cx).text_color(p.faint).child(
                                    Button::new(SharedString::from(format!("sign-in-{bridge}")))
                                        .h(px(TAP))
                                        .px(px(GUTTER))
                                        .label(t!("Sign in"))
                                        .on_click(cx.listener(move |this, _, _, cx| this.choose_network(&bridge, None, cx))),
                                ))
                            })
                            .children(network.logins.iter().map(|login| {
                                let (bridge, id) = (network.bridge.clone(), login.id.clone());
                                let ink = if bridges::needs_sign_in(&login.state) { p.negative } else { p.muted };
                                account_row(SharedString::from(format!("account-{}-{}", network.bridge, login.id)), &login.name, &plain(&login.state), ink, cx)
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_account(bridge.clone(), id.clone(), cx)))
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

    /// The account chosen on the Networks screen. Signing out is last, in red, and asked about first.
    fn render_network_account(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let found = self.account.as_ref().and_then(|(bridge, id)| {
            let network = self.networks.iter().flatten().find(|n| n.bridge == *bridge)?;
            Some((network.clone(), network.logins.iter().find(|l| l.id == *id)?.clone()))
        });
        let Some((network, login)) = found else {
            return div().text_size(fs(14.)).text_color(p.faint).child(t!("This account is no longer signed in.")).into_any_element();
        };
        let broken = bridges::needs_sign_in(&login.state);
        let again = {
            let (bridge, id) = (network.bridge.clone(), login.id.clone());
            cx.listener(move |this, _, _, cx| this.choose_network(&bridge, Some(id.clone()), cx))
        };
        let sign_out = {
            let (bridge, id, name, network) = (network.bridge.clone(), login.id.clone(), login.name.clone(), network.name.clone());
            cx.listener(move |this, _, _, cx| this.ask_sign_out(bridge.clone(), id.clone(), name.clone(), network.clone(), cx))
        };
        let servers = (network.bridge == DISCORD).then(|| self.render_guilds(&login.id, cx));
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                section(&network.name, cx).child(
                    card(cx).child(row(&login.name, "", cx).child(div().text_size(fs(14.)).text_color(if broken { p.negative } else { p.muted }).child(plain(&login.state)))),
                ),
            )
            .child(
                v_flex()
                    .gap(px(8.))
                    .map(|el| {
                        if broken {
                            el.child(h_flex().child(Button::new("account-sign-in").primary().h(px(TAP)).px(px(GUTTER + 4.)).label(t!("Sign in again")).on_click(again)))
                        } else {
                            el.child(card(cx).child(account_row("account-sign-in", t!("Sign in again"), "", p.muted, cx).on_click(again)))
                        }
                    })
                    .child(footnote(t!("Signing in again keeps this account's chats. On Messenger it is also when the bridge asks for the PIN of your encrypted chat history."), cx)),
            )
            .children(servers)
            .child(
                card(cx).child(
                    h_flex()
                        .id("account-sign-out")
                        .min_h(px(ROW_H))
                        .text_size(fs(16.))
                        .text_color(p.negative)
                        .role(Role::Button)
                        .cursor_pointer()
                        .on_click(sign_out)
                        .child(t!("Sign out")),
                ),
            )
            .into_any_element()
    }

    /// A Discord account's servers, each with a switch: the bridge brings in only those switched on.
    fn render_guilds(&mut self, login: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let list = match self.guilds.as_ref().filter(|(l, _)| l == login) {
            None => div().text_size(fs(14.)).text_color(p.faint).child(t!("Loading…")).into_any_element(),
            Some((_, list)) if list.is_empty() => div().text_size(fs(14.)).text_color(p.faint).child(t!("This account is in no server.")).into_any_element(),
            Some((_, list)) => card(cx)
                .children(list.iter().map(|g| {
                    let weak = cx.entity().downgrade();
                    let (login, id, name) = (login.to_owned(), g.id.clone(), g.name.clone());
                    let detail = if g.available { "" } else { t!("Discord reports it unavailable now") };
                    row(&g.name, detail, cx).child(
                        Switch::new(SharedString::from(format!("guild-{}", g.id)))
                            .checked(g.bridged)
                            .disabled(!g.available && !g.bridged)
                            .on_click(move |on, _, cx| {
                                let (login, id, name) = (login.clone(), id.clone(), name.clone());
                                let _ = weak.update(cx, |this, cx| {
                                    if *on {
                                        this.set_guild(DISCORD.to_owned(), login, id, true, cx);
                                    } else {
                                        this.ask_stop_guild(DISCORD.to_owned(), login, id, name, cx);
                                    }
                                });
                            }),
                    )
                }))
                .into_any_element(),
        };
        section(t!("Servers"), cx)
            .child(list)
            .child(footnote(t!("A server's channels come in as chats only while it is switched on."), cx))
            .into_any_element()
    }

    /// A quiet line over the chat list while a network's login needs signing in again; it opens Networks.
    pub(super) fn render_attention(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let text = match self.attention.as_slice() {
            [] => return None,
            [(bridge, _)] => t!("{network} needs you to sign in again.", network = bridges::display_name(bridge)),
            [(first, _), rest @ ..] if rest.iter().all(|(b, _)| b == first) => t!("{network} needs you to sign in again.", network = bridges::display_name(first)),
            _ => t!("Some networks need you to sign in again.").to_owned(),
        };
        Some(
            h_flex()
                .id("network-attention")
                .mx(px(GUTTER))
                .mt(px(4.))
                .mb(px(4.))
                .min_h(px(TAP))
                .px(px(GAP))
                .gap(px(GAP))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .role(Role::Button)
                .cursor_pointer()
                .active(|el| el.bg(p.raised))
                .on_click(cx.listener(|this, _, _, cx| this.open_panel(Panel::Networks, cx)))
                .child(div().flex_1().min_w_0().text_size(fs(14.)).line_height(fs(20.)).text_color(p.warning).child(text))
                .child(Icon::new(IconName::ChevronRight).size_4().flex_none().text_color(p.faint))
                .into_any_element(),
        )
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
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_network(bridge, None, cx)))
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
                        let state = cx.new(|cx| {
                            let mut input = InputState::new(window, cx).placeholder(field.name.clone()).masked(field.kind == "password");
                            input.set_value(field.default_value.clone(), window, cx);
                            input
                        });
                        // Codes, PINs and passwords must not be autocorrected.
                        let keyboard = match field.kind.as_str() {
                            "email" => Keyboard::Email,
                            _ if bridges::digits_only(&field.pattern) => Keyboard::Number,
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
                    .children(wanted.iter().zip(&self.fields).map(|(field, (id, input))| {
                        let choice = (field.kind == "select" && !field.options.is_empty()).then(|| {
                            let chosen = input.read(cx).value().to_string();
                            card(cx).children(field.options.iter().enumerate().map(|(ix, label)| {
                                let on = *label == chosen;
                                let (input, label) = (input.clone(), label.clone());
                                option((SharedString::from(format!("select-{id}")), ix), &label, "", on, cx).when(!on, |el| {
                                    el.on_click(cx.listener(move |_, _, window, cx| input.update(cx, |i, cx| i.set_value(label.clone(), window, cx))))
                                })
                            }))
                        });
                        v_flex()
                            .gap_1()
                            .child(div().text_size(fs(14.)).font_weight(FontWeight::MEDIUM).child(field.name.clone()))
                            .map(|el| match choice {
                                Some(choice) => el.child(choice),
                                None => el.child(Input::new(input)),
                            })
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
            StepKind::Cookies(wanted) => {
                let phone = cfg!(target_os = "android");
                // Values a browser keeps outside its cookies: pasted as JSON by these names.
                let other: Vec<String> = wanted.fields.iter().filter(|f| f.cookies.is_empty()).map(|f| format!("\"{}\"", f.id)).collect();
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
                    .when(!phone && !other.is_empty(), |el| {
                        el.child(note(&t!("This network also needs values the browser keeps outside its cookies. Paste everything as one JSON object with these names: {names}.", names = other.join(", "))))
                    })
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
            StepKind::Passkey => {
                let (bridge, relogin) = (step.bridge.clone(), self.relogin.clone());
                v_flex()
                    .gap_3()
                    .child(note(t!("This step asks for a passkey, which Unisend cannot use yet. Choose another way to sign in; on WhatsApp, use the code you type on your phone.")))
                    .child(h_flex().child(
                        Button::new("login-other-way")
                            .primary()
                            .label(t!("Choose another way"))
                            .on_click(cx.listener(move |this, _, _, cx| this.choose_network(&bridge, relogin.clone(), cx))),
                    ))
                    .into_any_element()
            }
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

/// A bridge or login state in plain words. The known states are mautrix bridgev2's
/// (`bridgev2/status/bridgestate.go`) and Beeper's `STOPPED`; a new one shows as its code in words.
fn plain(state: &str) -> String {
    let known = match state {
        "CONNECTED" => t!("Connected"),
        "RUNNING" => t!("Running"),
        "STARTING" | "CONNECTING" => t!("Connecting…"),
        "BACKFILLING" => t!("Fetching older chats…"),
        "TRANSIENT_DISCONNECT" => t!("No connection, retrying…"),
        "BAD_CREDENTIALS" => t!("Needs a new sign-in"),
        "LOGGED_OUT" => t!("Not signed in"),
        "UNKNOWN_ERROR" => t!("Not working"),
        "BRIDGE_UNREACHABLE" => t!("Beeper cannot reach it"),
        "UNCONFIGURED" => t!("Not set up yet"),
        "STOPPED" => t!("Turned off"),
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    let lower = state.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// A row that opens what it names: its label, a line under it in `ink`, and a chevron.
fn account_row(id: impl Into<ElementId>, label: &str, detail: &str, ink: Hsla, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .min_h(px(ROW_H))
        .py(px(8.))
        .mx(px(-8.))
        .px(px(8.))
        .gap(px(GAP))
        .rounded(px(12.))
        .role(Role::Button)
        .aria_label(label.to_owned())
        .cursor_pointer()
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
        .active(|el| el.bg(p.raised))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_size(fs(16.)).line_height(fs(22.)).child(label.to_owned()))
                .when(!detail.is_empty(), |el| el.child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(ink).child(detail.to_owned()))),
        )
        .child(Icon::new(IconName::ChevronRight).size_4().flex_none().text_color(p.faint))
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

/// What a setting's control needs explained at more length, under its card.
pub(super) fn footnote(text: &str, cx: &App) -> Div {
    div().px(px(GAP)).text_size(fs(13.)).line_height(fs(18.)).text_color(Palette::of(cx).muted).child(text.to_owned())
}

/// A row that leads somewhere, as a whole: its name, the value set now under it, and where it goes.
pub(super) fn link(id: impl Into<ElementId>, label: &'static str, value: SharedString, icon: Option<IconName>, trail: Option<IconName>, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    // A page's summary keeps to two lines; what an action does is read whole.
    let page = trail == Some(IconName::ChevronRight);
    h_flex()
        .id(id)
        .min_h(px(ROW_H))
        .py(px(8.))
        // The pressed fill reaches past the text, inside the card's padding.
        .mx(px(-8.))
        .px(px(8.))
        .gap(px(GAP))
        .rounded(px(12.))
        .role(Role::Button)
        .aria_label(label)
        .when(!value.is_empty(), |el| el.aria_value(value.clone()))
        .cursor_pointer()
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
        .active(|el| el.bg(p.raised))
        .children(icon.map(|icon| Icon::new(icon).size_5().flex_none().text_color(p.brand)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_size(fs(16.)).line_height(fs(22.)).child(label))
                .when(!value.is_empty(), |el| el.child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).when(page, |el| el.line_clamp(2)).child(value))),
        )
        .children(trail.map(|icon| Icon::new(icon).size_4().flex_none().text_color(p.faint)))
}

/// One answer among a setting's choices, checked when it is the one set.
fn option(id: impl Into<ElementId>, label: &str, detail: &str, on: bool, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    row(label, detail, cx)
        .id(id)
        .mx(px(-8.))
        .px(px(8.))
        .rounded(px(12.))
        .role(Role::RadioButton)
        .aria_label(label.to_owned())
        .aria_toggled(on.into())
        .when(!on, |el| el.cursor_pointer().when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised))).active(|el| el.bg(p.raised)))
        .when(on, |el| el.child(Icon::new(IconName::Check).size_5().flex_none().text_color(p.text)))
}

/// A list of a setting's choices; a tap saves the one tapped.
fn pick<T: Copy + PartialEq + 'static>(id: &'static str, all: impl IntoIterator<Item = T>, current: T, label: fn(T) -> &'static str, set: fn(&mut crate::Settings, T), cx: &mut Context<BeeperApp>) -> AnyElement {
    card(cx)
        .children(all.into_iter().enumerate().map(|(ix, choice)| {
            let on = current == choice;
            option((id, ix), label(choice), "", on, cx).when(!on, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    set(&mut this.settings, choice);
                    this.settings.save(&this.dir);
                    cx.notify();
                }))
            })
        }))
        .into_any_element()
}

fn lock_label(mode: LockMode) -> &'static str {
    match mode {
        LockMode::Off => t!("Off|app lock"),
        LockMode::Phone => t!("Phone lock"),
        LockMode::Pin => t!("App PIN"),
    }
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
    copying: bool,
    error: Option<String>,
}

impl BeeperApp {
    /// The font's name, or what is happening to it.
    fn emoji_value(&self) -> String {
        let name = self.settings.emoji_font.as_deref().unwrap_or(t!("System|emoji"));
        match () {
            _ if self.emoji.copying => t!("Copying the font…").to_owned(),
            _ if self.emoji.pending => t!("{name}. Applies after restart.", name = name),
            _ => name.to_owned(),
        }
    }

    fn render_emoji(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let chosen = self.settings.emoji_font.clone();
        let free = !self.emoji.copying;
        let options = card(cx)
            .child(
                option("emoji-system", t!("System|emoji"), "", chosen.is_none(), cx)
                    .when(chosen.is_some() && free, |el| el.on_click(cx.listener(|this, _, _, cx| this.use_system_emoji(cx)))),
            )
            .children(chosen.map(|family| option("emoji-own", &family, "", true, cx)));
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(options)
                    .when_some(self.emoji.error.clone(), |el, error| el.child(div().px(px(GAP)).text_size(fs(13.)).text_color(p.negative).child(error)))
                    .when(self.emoji.copying || self.emoji.pending, |el| el.child(footnote(&self.emoji_value(), cx))),
            )
            .child(
                card(cx).child(
                    link("emoji-file", t!("Choose font file…"), SharedString::default(), Some(IconName::File), None, cx)
                        .when(!free, |el| el.text_color(p.faint))
                        .when(free, |el| el.on_click(cx.listener(|this, _, _, cx| this.choose_emoji_font(cx)))),
                ),
            )
            .into_any_element()
    }

    fn choose_emoji_font(&mut self, cx: &mut Context<Self>) {
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
        self.emoji.error = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{plain, qr_image};

    #[test]
    fn qr_code_is_a_png_with_its_margin() {
        let image = qr_image("2@abcdefghijklmnopqrstuvwxyz,0123456789").unwrap();
        let decoded = image::load_from_memory(image.bytes()).unwrap().to_luma8();
        assert_eq!(decoded.width(), decoded.height());
        // White margin, then the finder pattern's dark corner.
        assert_eq!((decoded.get_pixel(0, 0).0, decoded.get_pixel(4 * 8, 4 * 8).0), ([255], [0]));
        assert_eq!(plain("BAD_CREDENTIALS"), "Needs a new sign-in");
    }
}
