//! Settings, the account's chat networks, and the login flow that adds one.

use std::collections::HashMap;

use gpui_kit::component::switch::Switch;

use super::*;
use crate::ThemeChoice;
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

    /// One level up: the login and the network list lead back to where they were opened from.
    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        self.panel = match self.panel {
            Some(Panel::Login) => {
                self.end_login(cx);
                Some(Panel::Networks)
            }
            Some(Panel::Add) => Some(Panel::Networks),
            Some(Panel::Networks) => Some(Panel::Settings),
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

    pub(super) fn render_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (title, body) = match panel {
            Panel::Settings => ("Settings", self.render_settings(cx).into_any_element()),
            Panel::Networks => ("Chat networks", self.render_networks(cx).into_any_element()),
            Panel::Add => ("Add an account", self.render_add(cx).into_any_element()),
            Panel::Login => ("Sign in", self.render_login_step(window, cx).into_any_element()),
        };
        v_flex()
            .size_full()
            .child(
                header(cx)
                    .gap_2()
                    .child(Button::new("panel-back").ghost().icon(IconName::ArrowLeft).tooltip("Back").on_click(cx.listener(
                        |this, _, _, cx| this.back(cx),
                    )))
                    .child(div().text_size(px(17.)).font_weight(FontWeight::BOLD).child(title)),
            )
            .child(div().id("panel-body").flex_1().min_h_0().overflow_y_scroll().child(v_flex().w_full().max_w(px(560.)).p_4().gap_5().child(body)))
            .into_any_element()
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.settings.theme;
        let choice = |label: &'static str, value: ThemeChoice, cx: &mut Context<Self>| {
            Button::new(label)
                .label(label)
                .when(theme == value, |b| b.primary())
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(value, window, cx)))
        };
        let themes = h_flex()
            .gap_2()
            .child(choice("Dark", ThemeChoice::Dark, cx))
            .child(choice("Light", ThemeChoice::Light, cx))
            .child(choice("System", ThemeChoice::System, cx));
        let weak = cx.entity().downgrade();
        let incognito = Switch::new("incognito").checked(self.settings.incognito).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_incognito(*on, cx));
        });
        v_flex()
            .gap_5()
            .child(section("Appearance", cx).child(row("Theme", "", cx).child(themes)))
            .child(section("Privacy", cx).child(
                row("Incognito mode", "Others do not see that you have read their messages.", cx).child(incognito),
            ))
            .child(
                section("Chat networks", cx).child(
                    row("Accounts", "WhatsApp, Messenger, Signal and the rest: see what is connected, add one, sign out.", cx).child(
                        Button::new("open-networks")
                            .label("Manage")
                            .on_click(cx.listener(|this, _, _, cx| this.open_panel(Panel::Networks, cx))),
                    ),
                ),
            )
            .child(section("Account", cx).child(
                row("Beeper", &self.user.as_ref().map(ToString::to_string).unwrap_or_default(), cx).child(
                    Button::new("sign-out").danger().label("Sign out").on_click(cx.listener(|this, _, _, cx| {
                        this.panel = None;
                        this.send(Cmd::SignOut, cx);
                    })),
                ),
            ))
            .child(div().text_size(px(12.)).text_color(cx.theme().muted_foreground).child(concat!("beeper-gpui ", env!("CARGO_PKG_VERSION"))))
    }

    fn render_networks(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let dim = cx.theme().muted_foreground;
        let list = match &self.networks {
            None => div().text_color(dim).child(if self.working { "Loading…" } else { "Could not load the networks." }).into_any_element(),
            Some(networks) if networks.is_empty() => div().text_color(dim).child("No chat networks are connected yet.").into_any_element(),
            Some(networks) => v_flex()
                .gap_3()
                .children(networks.iter().map(|network| {
                    let stopped = network.state != "RUNNING";
                    section(&network.name, cx)
                        .when(stopped, |el| el.child(div().text_size(px(13.)).text_color(dim).child(format!("Bridge: {}", pretty(&network.state)))))
                        .when(network.logins.is_empty() && !stopped, |el| el.child(div().text_size(px(13.)).text_color(dim).child("Not signed in")))
                        .children(network.logins.iter().map(|login| {
                            let (bridge, id) = (network.bridge.clone(), login.id.clone());
                            let ok = login.state == "CONNECTED";
                            row(&login.name, &pretty(&login.state), cx)
                                .when(!ok, |el| el.text_color(cx.theme().danger))
                                .child(Button::new(SharedString::from(format!("logout-{}-{}", network.bridge, login.id))).label("Sign out").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.working = this.send(Cmd::Logout { bridge: bridge.clone(), login: id.clone() }, cx);
                                    }),
                                ))
                        }))
                }))
                .into_any_element(),
        };
        v_flex()
            .gap_4()
            .child(list)
            .child(h_flex().child(Button::new("add-network").primary().label("Add an account").on_click(cx.listener(|this, _, _, cx| {
                this.open_panel(Panel::Add, cx);
            }))))
    }

    fn render_add(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(div().text_size(px(13.)).text_color(cx.theme().muted_foreground).child(
                "The account is added to Beeper itself, so every Beeper app you use gets it, not only this one.",
            ))
            .children(bridges::KNOWN.iter().map(|&(bridge, name)| {
                h_flex()
                    .id(bridge)
                    .h(px(52.))
                    .px_3()
                    .rounded(px(12.))
                    .bg(cx.theme().secondary)
                    .hover(|el| el.bg(cx.theme().secondary_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_network(bridge, cx)))
                    .child(div().flex_1().text_size(px(16.)).child(name))
                    .child(Icon::new(IconName::ChevronRight).size_4().text_color(cx.theme().muted_foreground))
            }))
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
                            other => keyboard_follows(other, keyboard),
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
                    (None, _) => el.child(div().text_size(px(28.)).font_weight(FontWeight::BOLD).child(data.clone())),
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
            .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(name))
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

fn section(title: &str, cx: &App) -> Div {
    v_flex().gap_2().child(
        div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().muted_foreground).child(title.to_owned()),
    )
}

/// A label with an optional explanation on the left; the caller adds the control on the right.
fn row(label: &str, detail: &str, cx: &App) -> Div {
    h_flex().gap_3().p_3().rounded(px(12.)).bg(cx.theme().secondary).child(
        v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_size(px(16.)).child(label.to_owned()))
            .when(!detail.is_empty(), |el| el.child(div().text_size(px(13.)).text_color(cx.theme().muted_foreground).child(detail.to_owned()))),
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
