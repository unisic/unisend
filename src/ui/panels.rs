//! Settings, the account's chat networks, and the login flow that adds one.

use std::collections::HashMap;

use gpui_kit::component::switch::Switch;

use super::*;
use crate::{ChatColour, ThemeChoice};

/// A settings row, and the widest a settings page grows.
const ROW_H: f32 = 52.;
const PANEL_W: f32 = 560.;
use crate::bridges::{self, Step, StepKind};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Panel {
    Settings,
    Networks,
    /// Choosing which network to add.
    Add,
    Login,
}

impl BeeperApp {
    pub(super) fn open_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Networks {
            self.working = self.send(Cmd::Networks, cx);
        }
        self.panel = Some(panel);
        cx.notify();
    }

    /// One level up: a login leads back to the networks, a destination back to the chats.
    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        self.panel = match self.panel {
            Some(Panel::Login) => {
                self.end_login(cx);
                Some(Panel::Networks)
            }
            Some(Panel::Add) => Some(Panel::Networks),
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
                self.error = Some(format!("The sign-in is not complete: the \"{missing}\" cookie is missing.").into());
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
            Panel::Settings => ("Settings", self.render_settings(cx).into_any_element()),
            Panel::Networks => ("Networks", self.render_networks(cx).into_any_element()),
            Panel::Add => ("Add an account", self.render_add(cx).into_any_element()),
            Panel::Login => ("Sign in", self.render_login_step(window, cx).into_any_element()),
        };
        // The destinations are reached from the navigation; only what lies under them leads back.
        let deep = matches!(panel, Panel::Add | Panel::Login);
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
                div().id("panel-body").flex_1().min_h_0().overflow_y_scroll().child(
                    v_flex().w_full().max_w(px(PANEL_W)).px(px(edge)).pt(px(8.)).pb(px(GAP * 2.)).gap(px(GAP * 2.)).child(body),
                ),
            )
            .into_any_element()
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let theme = self.settings.theme;
        let choice = |label: &'static str, value: ThemeChoice, cx: &mut Context<Self>| {
            let on = theme == value;
            div()
                .id(label)
                .h(px(36.))
                .px(px(GAP))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(px(14.))
                .text_color(if on { p.ground } else { p.muted })
                .when(on, |el| el.bg(p.text))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(value, window, cx)))
                .child(label)
        };
        let themes = h_flex()
            .flex_none()
            .p(px(4.))
            .rounded_full()
            .bg(p.raised)
            .child(choice("Dark", ThemeChoice::Dark, cx))
            .child(choice("Light", ThemeChoice::Light, cx))
            .child(choice("System", ThemeChoice::System, cx));
        let swatches = h_flex().flex_wrap().gap_x(px(4.)).pb(px(8.)).children(ChatColour::ALL.map(|colour| {
            let on = self.settings.chat_colour == colour;
            let name = format!("Chat colour: {}", colour.label());
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
        let user = self.user.as_ref().map(ToString::to_string).unwrap_or_default();
        v_flex()
            .gap(px(GAP * 2.))
            .child(section("Appearance", cx).child(
                card(cx)
                    .child(row("Theme", "", cx).child(themes))
                    .child(row("Chat colour", &format!("{}. Your own messages, on this device.", self.settings.chat_colour.label()), cx))
                    .child(swatches),
            ))
            .when(cfg!(target_os = "android"), |el| {
                el.child(section("Notifications", cx).child(card(cx).child(
                    row("Sounds and importance", "Direct messages, group chats and mentions each have their own.", cx).child(
                        h_flex()
                            .id("notification-settings")
                            .h(px(TAP))
                            .px(px(GAP))
                            .rounded_full()
                            .bg(Palette::of(cx).raised)
                            .cursor_pointer()
                            .on_click(|_, _, _| crate::notification_settings())
                            .child("Change"),
                    ),
                )))
            })
            .child(section("Privacy", cx).child(card(cx).child(
                row("Incognito", "Others do not see that you have read their messages.", cx).child(incognito),
            )))
            .child(
                section("Account", cx).child(
                    card(cx).child(row("Beeper", &user, cx)).child(
                        h_flex()
                            .id("sign-out")
                            .min_h(px(ROW_H))
                            .text_size(px(16.))
                            .text_color(p.negative)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                crate::background(false);
                                this.panel = None;
                                this.send(Cmd::SignOut, cx);
                            }))
                            .child("Sign out"),
                    ),
                ),
            )
            .child(div().text_size(px(12.)).text_color(p.faint).child(concat!("beeper-gpui ", env!("CARGO_PKG_VERSION"))))
    }

    fn render_networks(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let note = |text: &'static str| div().text_size(px(14.)).text_color(p.faint).child(text);
        let list = match &self.networks {
            None => note(if self.working { "Loading..." } else { "Could not load the networks." }).into_any_element(),
            Some(networks) if networks.is_empty() => note("No chat network is connected yet.").into_any_element(),
            Some(networks) => v_flex()
                .gap(px(GAP * 2.))
                .children(networks.iter().map(|network| {
                    let stopped = network.state != "RUNNING";
                    section(&network.name, cx).child(
                        card(cx)
                            .when(stopped, |el| el.child(row(&format!("Bridge {}", pretty(&network.state).to_lowercase()), "", cx).text_color(p.warning)))
                            .when(network.logins.is_empty() && !stopped, |el| el.child(row("Not signed in", "", cx).text_color(p.faint)))
                            .children(network.logins.iter().map(|login| {
                                let (bridge, id) = (network.bridge.clone(), login.id.clone());
                                let ok = login.state == "CONNECTED";
                                row(&login.name, &pretty(&login.state), cx).when(!ok, |el| el.text_color(p.negative)).child(
                                    h_flex()
                                        .id(SharedString::from(format!("logout-{}-{}", network.bridge, login.id)))
                                        .h(px(TAP))
                                        .flex_none()
                                        .text_size(px(14.))
                                        .text_color(p.negative)
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.working = this.send(Cmd::Logout { bridge: bridge.clone(), login: id.clone() }, cx);
                                        }))
                                        .child("Sign out"),
                                )
                            })),
                    )
                }))
                .into_any_element(),
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(h_flex().child(Button::new("add-network").primary().h(px(TAP)).px(px(GUTTER + 4.)).label("Add an account").on_click(cx.listener(|this, _, _, cx| {
                this.open_panel(Panel::Add, cx);
            }))))
            .child(list)
    }

    fn render_add(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        v_flex()
            .gap(px(GAP))
            .child(div().text_size(px(14.)).line_height(px(20.)).text_color(p.muted).child(
                "The account is added to Beeper itself, so every Beeper app you use gets it, not only this one.",
            ))
            .child(card(cx).children(bridges::KNOWN.iter().map(|&(bridge, name)| {
                h_flex()
                    .id(bridge)
                    .h(px(ROW_H))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_network(bridge, cx)))
                    .child(div().flex_1().text_size(px(16.)).child(name))
                    .child(Icon::new(IconName::ChevronRight).size_4().text_color(p.faint))
            })))
    }

    fn render_login_step(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dim = cx.theme().muted_foreground;
        let note = |text: &str| div().text_size(px(14.)).text_color(dim).child(text.to_owned());
        let Some(step) = self.step.clone() else {
            // Before the first step: starting the bridge, or choosing between ways to sign in.
            return match &self.flows {
                Some((bridge, flows)) => v_flex()
                    .gap_2()
                    .child(note("Choose how to sign in."))
                    .children(flows.iter().map(|flow| {
                        let (bridge, id) = (bridge.clone(), flow.id.clone());
                        v_flex()
                            .id(SharedString::from(flow.id.clone()))
                            .p_3()
                            .rounded(px(12.))
                            .bg(cx.theme().secondary)
                            .hover(|el| el.bg(cx.theme().secondary_hover))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.start_flow(bridge.clone(), id.clone(), cx)))
                            .child(div().text_size(px(16.)).child(flow.name.clone()))
                            .child(div().text_size(px(13.)).text_color(dim).child(flow.description.clone()))
                    }))
                    .into_any_element(),
                None => note(if self.working { "Starting… this can take a minute." } else { "Could not start the sign-in." }).into_any_element(),
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
                            .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(field.name.clone()))
                            .child(Input::new(input))
                            .when(!field.description.is_empty(), |el| el.child(note(&field.description)))
                    }))
                    .child(h_flex().child(
                        Button::new("login-continue")
                            .primary()
                            .label("Continue")
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
                        "A browser opens for you to sign in. It closes by itself when you are in."
                    } else {
                        "Sign in on the page that opens in your browser. Then copy the request's cookies from the \
                         browser's developer tools (Network, any request to the site, Copy as cURL) and paste them here."
                    }))
                    .child(h_flex().child(
                        Button::new("login-browser")
                            .primary()
                            .label(if phone { "Sign in" } else { "Open the sign-in page" })
                            .disabled(self.working)
                            .on_click(cx.listener(|this, _, _, cx| this.browser_login(cx))),
                    ))
                    .child(Input::new(&self.cookie_paste))
                    .child(h_flex().child(
                        Button::new("login-cookies")
                            .label("Continue with pasted cookies")
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
                    (None, _) => el.child(div().text_size(px(28.)).font_weight(FontWeight::SEMIBOLD).child(data.clone())),
                })
                .child(note("Waiting for the other device…"))
                .into_any_element(),
            StepKind::Done => v_flex()
                .gap_3()
                .child(div().text_size(px(16.)).child(format!("{name} is connected.")))
                .child(note("Its chats appear in the list as the bridge brings them in."))
                .child(h_flex().child(Button::new("login-done").primary().label("Done").on_click(cx.listener(|this, _, _, cx| this.back(cx)))))
                .into_any_element(),
        };
        v_flex()
            .gap_4()
            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(name))
            .when(!step.instructions.is_empty(), |el| el.child(div().text_size(px(15.)).child(step.instructions.clone())))
            .child(body)
            .into_any_element()
    }
}

/// "BAD_CREDENTIALS" as "Bad credentials".
fn pretty(state: &str) -> String {
    let lower = state.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// A titled group of settings.
fn section(title: &str, cx: &App) -> Div {
    let p = Palette::of(cx);
    v_flex().gap(px(8.)).child(div().px(px(GAP)).text_size(px(12.)).text_color(p.faint).child(title.to_owned()))
}

/// The rows of one group, on one surface.
fn card(cx: &App) -> Div {
    v_flex().px(px(GAP)).py(px(4.)).rounded(px(RADIUS)).bg(Palette::of(cx).surface)
}

/// A setting: what it is, a line about it, and its control after it.
fn row(label: &str, detail: &str, cx: &App) -> Div {
    let p = Palette::of(cx);
    h_flex().min_h(px(ROW_H)).py(px(8.)).gap(px(GAP)).child(
        v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_size(px(16.)).line_height(px(22.)).child(label.to_owned()))
            .when(!detail.is_empty(), |el| el.child(div().text_size(px(13.)).line_height(px(18.)).text_color(p.muted).child(detail.to_owned()))),
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
