//! Settings' Devices section and the sheet that verifies this device by comparing emoji.

use crate::backend::verify::{Auth, CrossSigning, Devices, NAME_MAX, Proof, Trust, Verify};

use super::*;

/// A device being signed out: what the server asks for next (`None` while that is found out).
struct SignOut {
    device: String,
    need: Option<Auth>,
    busy: bool,
    error: Option<SharedString>,
}

pub(super) struct Ui {
    devices: Option<Devices>,
    loading: bool,
    failed: Option<SharedString>,
    renaming: bool,
    name: Entity<InputState>,
    out: Option<SignOut>,
    /// Cleared once sent.
    password: Entity<InputState>,
    code: Entity<InputState>,
    /// What the sheet shows; `None` closes it.
    sheet: Option<Verify>,
    /// Drawn while the sheet falls.
    sheet_last: Option<Verify>,
}

impl Ui {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Device name"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Code"));
        subs.push(cx.subscribe_in(&name, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.save_name(window, cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        for input in [&password, &code] {
            subs.push(cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { shift: false, .. } => this.send_secret(window, cx),
                other => keyboard_follows(other, Keyboard::Key, cx),
            }));
        }
        Self { devices: None, loading: false, failed: None, renaming: false, name, out: None, password, code, sheet: None, sheet_last: None }
    }
}

impl BeeperApp {
    pub(super) fn load_devices(&mut self, cx: &mut Context<Self>) {
        self.verify.loading = self.send(Cmd::Devices, cx);
        self.verify.failed = None;
    }

    pub(super) fn on_verify_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        let v = &mut self.verify;
        match ev {
            Ev::Devices(devices) => {
                v.devices = Some(devices);
                (v.loading, v.renaming, v.failed) = (false, false, None);
            }
            Ev::DevicesFailed(e) => (v.loading, v.failed) = (false, Some(e.into())),
            // The end of a verification the user already closed stays closed.
            Ev::Verify(state @ (Verify::Done | Verify::Failed(_))) => {
                if v.sheet.is_some() {
                    v.sheet = Some(state);
                }
            }
            Ev::Verify(state) => v.sheet = Some(state),
            Ev::SignOutStep(step) => {
                if let Some(out) = &mut v.out {
                    out.busy = false;
                    match step {
                        Ok(None) => v.out = None,
                        Ok(Some(need)) => (out.need, out.error) = (Some(need), None),
                        // A first answer that failed leaves nothing to do but close.
                        Err(e) => (out.need, out.error) = (out.need.clone().or(Some(Auth::Unsupported)), Some(e.into())),
                    }
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Escape and Android's back: the sheet's way out, when it is open.
    pub(super) fn verify_back(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = &self.verify.sheet else {
            return false;
        };
        if !matches!(state, Verify::Done | Verify::Failed(_)) {
            self.send(Cmd::VerifyCancel, cx);
        }
        self.verify.sheet = None;
        cx.notify();
        true
    }

    fn start_verify(&mut self, cx: &mut Context<Self>) {
        if self.send(Cmd::VerifyStart, cx) {
            self.verify.sheet = Some(Verify::Waiting);
        }
    }

    fn verify_act(&mut self, cmd: Cmd, next: Option<Verify>, cx: &mut Context<Self>) {
        if self.send(cmd, cx) {
            self.verify.sheet = next;
        }
    }

    fn edit_name(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.verify.renaming = on;
        if on {
            let current = self.verify.devices.as_ref().and_then(|d| d.list.iter().find(|d| d.this)).map(|d| d.name.clone()).unwrap_or_default();
            self.verify.name.update(cx, |input, cx| {
                input.set_value(current, window, cx);
                input.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn save_name(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.verify.name.read(cx).value().trim().to_string();
        if name.is_empty() || name.chars().count() > NAME_MAX {
            self.verify.failed = Some(format!("A device name has 1 to {NAME_MAX} characters.").into());
            cx.notify();
            return;
        }
        self.verify.failed = None;
        self.send(Cmd::RenameDevice(name), cx);
    }

    fn sign_out_device(&mut self, device: String, proof: Proof, cx: &mut Context<Self>) {
        let first = matches!(proof, Proof::Ask);
        if self.send(Cmd::SignOutDevice { device: device.clone(), proof }, cx) {
            match &mut self.verify.out {
                Some(out) if !first && out.device == device => (out.busy, out.error) = (true, None),
                _ => self.verify.out = Some(SignOut { device, need: None, busy: true, error: None }),
            }
        }
    }

    fn send_secret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(out) = &self.verify.out else { return };
        if out.busy {
            return;
        }
        let input = match out.need {
            Some(Auth::Password) => self.verify.password.clone(),
            Some(Auth::CodeSent) => self.verify.code.clone(),
            _ => return,
        };
        let secret = input.read(cx).value().to_string();
        if secret.trim().is_empty() {
            return;
        }
        let proof = if out.need == Some(Auth::Password) { Proof::Password(secret) } else { Proof::Code(secret) };
        let device = out.device.clone();
        // The secret lives on only in the request just sent.
        input.update(cx, |s, cx| s.set_value("", window, cx));
        self.sign_out_device(device, proof, cx);
    }

    fn close_sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.verify.out = None;
        for input in [&self.verify.password, &self.verify.code] {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        cx.notify();
    }

    pub(super) fn render_devices(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let section = panels::section("Devices", cx);
        let Some(devices) = self.verify.devices.clone() else {
            let line = match (&self.verify.failed, self.verify.loading) {
                (Some(e), _) => e.clone(),
                (None, true) => "Loading your devices".into(),
                (None, false) => SharedString::default(),
            };
            return section.child(panels::card(cx).child(panels::row("Your devices", &line, cx).when(!self.verify.loading, |el| {
                el.child(pill("devices-retry", "Try again", p.text, cx).on_click(cx.listener(|this, _, _, cx| this.load_devices(cx))))
            })));
        };
        let this_status = if devices.this_verified {
            "Verified. Your other devices trust this one."
        } else {
            "Not verified. Compare emoji with another device where you are signed in, or unlock with your recovery code."
        };
        let keys = match devices.cross_signing {
            CrossSigning::Here => "Set up. This device holds the keys, so it can verify your other devices.",
            CrossSigning::Elsewhere => "Set up on your account. This device does not hold the keys.",
            CrossSigning::Missing => "Not set up. Set it up in Beeper or Element, then verify this device here.",
        };
        let can_verify = !devices.this_verified && devices.cross_signing != CrossSigning::Missing;
        let rows = devices.list.iter().map(|d| self.device_row(d, cx)).collect::<Vec<_>>();
        section
            .child(
                panels::card(cx)
                    .child(panels::row("This device", this_status, cx).when(can_verify, |el| {
                        el.child(pill("verify-start", "Verify", p.text, cx).on_click(cx.listener(|this, _, _, cx| this.start_verify(cx))))
                    }))
                    .child(panels::row("Cross-signing", keys, cx)),
            )
            .child(panels::card(cx).children(rows))
            .children(self.verify.failed.clone().map(|e| div().px(px(GAP)).text_size(px(14.)).text_color(p.negative).child(e)))
    }

    fn device_row(&self, d: &crate::backend::verify::Device, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let trust = match d.trust {
            Trust::Verified => "Verified",
            Trust::Unverified => "Not verified",
            Trust::NoKeys => "No encryption",
        };
        let detail = match (d.this, d.last_seen) {
            (true, _) => format!("This device · {trust}"),
            (false, Some(ts)) => format!("{} · {trust}", seen(ts)),
            (false, None) => trust.to_owned(),
        };
        if d.this && self.verify.renaming {
            return v_flex()
                .py(px(8.))
                .gap(px(8.))
                .child(Input::new(&self.verify.name))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(pill("rename-save", "Save", p.text, cx).on_click(cx.listener(|this, _, window, cx| this.save_name(window, cx))))
                        .child(pill("rename-cancel", "Cancel", p.muted, cx).on_click(cx.listener(|this, _, window, cx| this.edit_name(false, window, cx)))),
                )
                .into_any_element();
        }
        let id = d.id.clone();
        let action = if d.this {
            pill("rename", "Rename", p.text, cx).on_click(cx.listener(|this, _, window, cx| this.edit_name(true, window, cx)))
        } else {
            pill(SharedString::from(format!("sign-out-{id}")), "Sign out", p.negative, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.sign_out_device(id.clone(), Proof::Ask, cx)))
        };
        let out = self.verify.out.as_ref().filter(|o| o.device == d.id);
        v_flex().child(panels::row(&d.name, &detail, cx).child(action)).children(out.map(|o| self.sign_out_form(o, cx))).into_any_element()
    }

    /// What signing out a device asks for, under its row.
    fn sign_out_form(&self, out: &SignOut, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let text = |s: String| div().text_size(px(14.)).line_height(px(20.)).text_color(p.muted).child(s);
        let mut form = v_flex().pb(px(12.)).gap(px(8.));
        let device = out.device.clone();
        let (line, field, button): (String, bool, Option<(&'static str, Proof)>) = match &out.need {
            None => ("Asking the server what it needs to sign the device out.".into(), false, None),
            Some(Auth::Password) => ("Enter your password to sign this device out.".into(), true, None),
            Some(Auth::Code { email }) => (format!("Beeper emails a code to {email} to confirm it is you."), false, Some(("Email me a code", Proof::SendCode))),
            Some(Auth::CodeSent) => ("Enter the code from the email.".into(), true, None),
            Some(Auth::Unsupported) => {
                ("This server asks for a kind of confirmation Murmur does not offer. Sign the device out from Beeper or Element.".into(), false, None)
            }
        };
        form = form.child(text(line));
        if out.need == Some(Auth::Password) {
            form = form.child(Input::new(&self.verify.password).mask_toggle());
        } else if field {
            form = form.child(Input::new(&self.verify.code));
        }
        let mut buttons = h_flex().gap(px(8.));
        if field {
            buttons = buttons.child(
                Button::new("sign-out-confirm")
                    .primary()
                    .h(px(TAP))
                    .label("Sign out")
                    .loading(out.busy)
                    .disabled(out.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.send_secret(window, cx))),
            );
        } else if let Some((label, _)) = &button {
            buttons = buttons.child(
                Button::new("sign-out-code")
                    .primary()
                    .h(px(TAP))
                    .label(*label)
                    .loading(out.busy)
                    .disabled(out.busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.sign_out_device(device.clone(), Proof::SendCode, cx))),
            );
        }
        let close = if out.need == Some(Auth::Unsupported) { "Close" } else { "Cancel" };
        buttons = buttons
            .child(Button::new("sign-out-cancel").ghost().h(px(TAP)).label(close).on_click(cx.listener(|this, _, window, cx| this.close_sign_out(window, cx))));
        form.child(buttons).children(out.error.clone().map(|e| div().text_size(px(14.)).text_color(p.negative).child(e)))
    }

    /// Rises like the chat's sheet. A tap outside closes it only once nothing is running, so a
    /// stray tap does not cancel a comparison.
    pub(super) fn render_verify_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.verify.sheet.is_some();
        if live {
            self.verify.sheet_last = self.verify.sheet.clone();
        } else if self.verify.sheet_last.is_none() {
            return None;
        }
        let motion =
            if live { Transition::new(motion::SHEET_IN).easing(motion::EASE_SHEET) } else { Transition::new(motion::SHEET_OUT).easing(motion::EASE_OUT) };
        let shown = Presence::new("verify-sheet", live).transition(motion).sample(window, cx);
        if !shown.should_render() {
            self.verify.sheet_last = None;
            return None;
        }
        let state = self.verify.sheet_last.clone()?;
        let p = Palette::of(cx);
        let (_, bottom) = crate::insets();
        let (title, body, primary, secondary): (&str, String, Option<Primary>, (&str, Option<Cmd>)) = match &state {
            Verify::Incoming { who } => (
                "Verification request",
                format!("{who} asks to verify. Accept only if you started this yourself."),
                Some(("Accept", Cmd::VerifyAccept, Some(Verify::Waiting))),
                ("Decline", Some(Cmd::VerifyCancel)),
            ),
            Verify::Waiting => (
                "Waiting for the other device",
                "Open Beeper, Element or Murmur where you are signed in and accept the request there.".into(),
                None,
                ("Cancel", Some(Cmd::VerifyCancel)),
            ),
            Verify::Emoji(_) => (
                "Compare emoji",
                "Check that the other device shows the same emoji in the same order.".into(),
                Some(("They match", Cmd::VerifyMatch(true), Some(Verify::Confirmed))),
                ("They don't match", Some(Cmd::VerifyMatch(false))),
            ),
            Verify::Confirmed => {
                ("Waiting for the other device", "Confirm on the other device that the emoji match.".into(), None, ("Cancel", Some(Cmd::VerifyCancel)))
            }
            Verify::Done => {
                ("Verified", "Both sides confirmed the same emoji. Messages this device could not read may now unlock.".into(), None, ("Done", None))
            }
            Verify::Failed(why) => ("Not verified", why.clone(), None, ("Close", None)),
        };
        let settled = matches!(state, Verify::Done | Verify::Failed(_));
        let emoji = match &state {
            Verify::Emoji(list) => Some(h_flex().flex_wrap().justify_center().gap_y(px(12.)).py(px(8.)).children(list.iter().map(|(symbol, name)| {
                v_flex()
                    .w(px(80.))
                    .items_center()
                    .gap(px(4.))
                    .child(div().text_size(px(32.)).line_height(px(40.)).child(symbol.clone()))
                    .child(div().text_size(px(12.)).text_color(p.muted).child(name.clone()))
            }))),
            _ => None,
        };
        // Its height, for the slide: title, two lines of text, the emoji, the buttons, the padding.
        let height = 8. + 36. + 48. + if emoji.is_some() { 150. } else { 0. } + (TAP + 8.) * (1 + primary.is_some() as usize) as f32 + bottom + 24.;
        let mut buttons = v_flex().gap(px(8.)).pt(px(8.));
        if let Some((label, cmd, next)) = primary {
            let cmd = std::cell::Cell::new(Some(cmd));
            buttons = buttons.child(Button::new("verify-primary").primary().h(px(TAP)).w_full().label(label).when(live, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(cmd) = cmd.take() {
                        this.verify_act(cmd, next.clone(), cx);
                    }
                }))
            }));
        }
        let (label, cmd) = secondary;
        let cmd = std::cell::Cell::new(cmd);
        let ink = if matches!(state, Verify::Emoji(_)) { p.negative } else { p.text };
        buttons = buttons.child(Button::new("verify-secondary").ghost().h(px(TAP)).w_full().text_color(ink).label(label).when(live, |el| {
            el.on_click(cx.listener(move |this, _, _, cx| {
                match cmd.take() {
                    // Saying they do not match ends it on both sides; the sheet stays to say so.
                    Some(Cmd::VerifyMatch(false)) => {
                        let ended = Verify::Failed("The emoji did not match, so nothing was verified.".into());
                        this.verify_act(Cmd::VerifyMatch(false), Some(ended), cx);
                    }
                    Some(cmd) => this.verify_act(cmd, None, cx),
                    None => this.verify_act_close(cx),
                }
            }))
        }));
        Some(
            div()
                .id("verify-sheet")
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_end()
                .items_center()
                .bg(gpui_kit::black().opacity(0.6 * shown.progress))
                .when(live, |el| {
                    el.occlude().on_click(cx.listener(move |this, _, _, cx| {
                        if settled {
                            this.verify_act_close(cx);
                        }
                    }))
                })
                .child(
                    v_flex()
                        .id("verify-sheet-body")
                        .relative()
                        .top(px(height * (1. - shown.progress)))
                        .w_full()
                        .max_w(px(SHEET_W))
                        .px(px(GUTTER + 4.))
                        .pt(px(8.))
                        .pb(px(bottom + 16.))
                        .rounded_t(px(RADIUS))
                        .bg(p.surface)
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(div().h(px(36.)).flex().items_center().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(title.to_owned()))
                        .child(div().text_size(px(14.)).line_height(px(20.)).text_color(p.muted).child(body))
                        .children(emoji)
                        .child(buttons),
                )
                .into_any_element(),
        )
    }

    fn verify_act_close(&mut self, cx: &mut Context<Self>) {
        self.verify.sheet = None;
        cx.notify();
    }
}

/// The sheet's main button: its label, what it sends, and what the sheet shows next.
type Primary = (&'static str, Cmd, Option<Verify>);

/// A small labelled control inside a settings row, at a finger's height.
fn pill(id: impl Into<ElementId>, label: &'static str, ink: Hsla, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex().id(id).flex_none().h(px(TAP)).px(px(GAP)).rounded_full().bg(p.raised).text_size(px(14.)).text_color(ink).cursor_pointer().child(label)
}

/// "Active today", "Active yesterday", "Active on 3 May".
fn seen(ts_ms: u64) -> String {
    let Some(t) = local(ts_ms) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    match (today - t.date_naive()).num_days() {
        ..=0 => "Active today".to_owned(),
        1 => "Active yesterday".to_owned(),
        _ if t.year() == today.year() => t.format("Active on %-d %B").to_string(),
        _ => t.format("Active on %-d %B %Y").to_string(),
    }
}
