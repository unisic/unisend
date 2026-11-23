//! Settings' Devices section and the sheet that verifies this device by comparing emoji.

use crate::backend::verify::{Auth, CrossSigning, Devices, NAME_MAX, Proof, Trust, Verify};

use super::sheet::{self, SheetMotion};
use super::*;
use crate::i18n::t;

/// A device being signed out: what the server asks for next (`None` while that is found out).
struct SignOut {
    device: String,
    need: Option<Auth>,
    busy: bool,
    error: Option<SharedString>,
}

/// The recovery code row: the code stays off the screen until asked for, and leaves it with the page.
#[derive(Default)]
enum Recovery {
    #[default]
    Hidden,
    Busy,
    Shown { code: SharedString, copied: bool },
    /// None is kept on this device.
    Absent,
    /// A new code was asked for; it waits for a second press, since it retires the old one.
    Confirm,
    Failed(SharedString),
}

pub(super) struct Ui {
    pub(super) devices: Option<Devices>,
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
    motion: SheetMotion,
    recovery: Recovery,
}

impl Ui {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Device name")));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Password")).masked(true));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Code")));
        crate::ime::follow(&name, window, cx);
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
        Self { devices: None, loading: false, failed: None, renaming: false, name, out: None, password, code, sheet: None, sheet_last: None, motion: SheetMotion::default(), recovery: Recovery::Hidden }
    }
}

/// Each verification emoji appears this long after the one before it.
const EMOJI_STEP: Duration = Duration::from_millis(40);

impl BeeperApp {
    pub(super) fn load_devices(&mut self, cx: &mut Context<Self>) {
        self.verify.loading = self.send(Cmd::Devices, cx);
        (self.verify.failed, self.verify.recovery) = (None, Recovery::Hidden);
    }

    fn recovery_ask(&mut self, cmd: Cmd, cx: &mut Context<Self>) {
        if self.send(cmd, cx) {
            self.verify.recovery = Recovery::Busy;
        }
        cx.notify();
    }

    fn recovery_set(&mut self, to: Recovery, cx: &mut Context<Self>) {
        self.verify.recovery = to;
        cx.notify();
    }

    pub(super) fn on_verify_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        let v = &mut self.verify;
        match ev {
            Ev::Devices(devices) => {
                v.devices = Some(devices);
                (v.loading, v.renaming, v.failed) = (false, false, None);
            }
            Ev::DevicesFailed(e) => (v.loading, v.failed) = (false, Some(e.into())),
            Ev::RecoveryCode(kept) => {
                v.recovery = match kept {
                    Ok(Some(code)) => Recovery::Shown { code: code.into(), copied: false },
                    Ok(None) => Recovery::Absent,
                    Err(e) => Recovery::Failed(e.into()),
                }
            }
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
            self.verify.failed = Some(t!("A device name has 1 to {max} characters.", max = NAME_MAX).into());
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

    /// Whether this device is verified, for the settings list.
    pub(super) fn devices_summary(&self) -> SharedString {
        match &self.verify.devices {
            Some(d) if d.this_verified => t!("This device is verified").into(),
            Some(_) => t!("This device is not verified").into(),
            None if self.verify.loading => t!("Loading your devices").into(),
            None => SharedString::default(),
        }
    }

    pub(super) fn render_devices(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let section = v_flex().gap(px(GAP * 2.));
        let Some(devices) = self.verify.devices.clone() else {
            let line = match (&self.verify.failed, self.verify.loading) {
                (Some(e), _) => e.clone(),
                (None, true) => t!("Loading your devices").into(),
                (None, false) => SharedString::default(),
            };
            return section.child(panels::card(cx).child(panels::row(t!("Your devices"), &line, cx).when(!self.verify.loading, |el| {
                el.child(pill("devices-retry", t!("Try again"), p.text, cx).on_click(cx.listener(|this, _, _, cx| this.load_devices(cx))))
            })));
        };
        let this_status = if devices.this_verified {
            t!("Verified. Your other devices trust this one.")
        } else {
            t!("Not verified. Compare emoji with another device where you are signed in, or unlock with your recovery code.")
        };
        let keys = match devices.cross_signing {
            CrossSigning::Here => t!("Set up. This device holds the keys, so it can verify your other devices."),
            CrossSigning::Elsewhere => t!("Set up on your account. This device does not hold the keys."),
            CrossSigning::Missing => t!("Not set up. Set it up in Beeper or Element, then verify this device here."),
        };
        let can_verify = !devices.this_verified && devices.cross_signing != CrossSigning::Missing;
        let rows = devices.list.iter().map(|d| self.device_row(d, cx)).collect::<Vec<_>>();
        section
            .child(
                panels::card(cx)
                    .child(panels::row(t!("This device"), this_status, cx).when(can_verify, |el| {
                        el.child(pill("verify-start", t!("Verify"), p.text, cx).on_click(cx.listener(|this, _, _, cx| this.start_verify(cx))))
                    }).when(self.need_recovery, |el| {
                        // The code goes in the chat list's unlock row, open and back even after "Later".
                        el.child(pill("recovery-unlock", t!("Unlock"), p.text, cx).on_click(cx.listener(|this, _, window, cx| {
                            (this.recovery_later, this.recovery_open) = (false, true);
                            this.set_tab(Tab::Chats, window, cx);
                        })))
                    }))
                    .child(panels::row(t!("Cross-signing"), keys, cx)),
            )
            .child(panels::card(cx).child(self.recovery_row(devices.cross_signing == CrossSigning::Here, cx)))
            .child(panels::card(cx).children(rows))
            .children(self.verify.failed.clone().map(|e| div().px(px(GAP)).text_size(fs(14.)).text_color(p.negative).child(e)))
    }

    /// The account's recovery code, to copy here and paste on another device. `holds` says this
    /// device has the keys a new code would lock away.
    fn recovery_row(&self, holds: bool, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let label = t!("Recovery code");
        let to = |state: fn() -> Recovery| cx.listener(move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx| this.recovery_set(state(), cx));
        match &self.verify.recovery {
            // Locked out of the keys, so no code is kept here: no "Show" that only says so.
            Recovery::Hidden if self.need_recovery => {
                panels::row(label, t!("None is kept on this device. Unlock it with your code once and it is kept here."), cx)
            }
            Recovery::Hidden => panels::row(label, t!("Unlocks encrypted messages on another device. Copy it here, paste it there."), cx)
                .child(pill("recovery-show", t!("Show|recovery code"), p.text, cx).on_click(cx.listener(|this, _, _, cx| this.recovery_ask(Cmd::RecoveryCode, cx)))),
            Recovery::Busy => panels::row(label, t!("Loading…"), cx),
            Recovery::Shown { code, copied } => {
                let copy = code.to_string();
                v_flex()
                    .py(px(8.))
                    .gap(px(8.))
                    .child(div().text_size(fs(16.)).line_height(fs(22.)).child(label))
                    .child(div().font_family(super::rich::MONO).text_size(fs(15.)).line_height(fs(22.)).child(code.clone()))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(pill("recovery-copy", if *copied { t!("Copied") } else { t!("Copy") }, p.text, cx).on_click(cx.listener(move |this, _, _, cx| {
                                crate::copy(cx, copy.clone());
                                // A desktop clipboard has no "sensitive" mark: the code leaves it after a minute
                                // unless something else was copied since. A clipboard history may still keep it.
                                #[cfg(not(target_os = "android"))]
                                {
                                    let code = copy.clone();
                                    cx.spawn(async move |_, cx| {
                                        cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
                                        cx.update(|cx| {
                                            if crate::paste(cx).as_deref() == Some(code.as_str()) {
                                                crate::copy(cx, String::new());
                                            }
                                        });
                                    })
                                    .detach();
                                }
                                if let Recovery::Shown { copied, .. } = &mut this.verify.recovery {
                                    *copied = true;
                                }
                                cx.notify();
                            })))
                            .child(pill("recovery-hide", t!("Hide"), p.text, cx).on_click(to(|| Recovery::Hidden))),
                    )
            }
            Recovery::Absent if holds => panels::row(label, t!("None is kept on this device. Make a new one here; the old one stops working."), cx)
                .child(pill("recovery-new", t!("New code"), p.text, cx).on_click(to(|| Recovery::Confirm))),
            Recovery::Absent => panels::row(label, t!("None is kept on this device. Unlock it with your code once and it is kept here."), cx),
            Recovery::Confirm => panels::row(label, t!("The old code stops working in every app, Beeper too. Keep the new one somewhere safe."), cx)
                .child(pill("recovery-cancel", t!("Cancel"), p.text, cx).on_click(to(|| Recovery::Absent)))
                .child(pill("recovery-make", t!("Make it"), p.negative, cx).on_click(cx.listener(|this, _, _, cx| this.recovery_ask(Cmd::NewRecoveryCode, cx)))),
            Recovery::Failed(e) => panels::row(label, e, cx)
                .child(pill("recovery-retry", t!("Try again"), p.text, cx).on_click(to(|| Recovery::Hidden))),
        }
    }

    fn device_row(&self, d: &crate::backend::verify::Device, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let trust = match d.trust {
            Trust::Verified => t!("Verified"),
            Trust::Unverified => t!("Not verified"),
            Trust::NoKeys => t!("No encryption"),
        };
        let detail = match (d.this, d.last_seen) {
            (true, _) => format!("{} · {trust}", t!("This device")),
            (false, Some(ts)) => format!("{} · {trust}", seen(ts)),
            (false, None) => trust.to_owned(),
        };
        if d.this && self.verify.renaming {
            return v_flex()
                .py(px(8.))
                .gap(px(8.))
                .child(Input::new(&self.verify.name))
                .child(crate::ime::marks(&self.verify.name))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(pill("rename-save", t!("Save"), p.text, cx).on_click(cx.listener(|this, _, window, cx| this.save_name(window, cx))))
                        .child(pill("rename-cancel", t!("Cancel"), p.muted, cx).on_click(cx.listener(|this, _, window, cx| this.edit_name(false, window, cx)))),
                )
                .into_any_element();
        }
        let id = d.id.clone();
        let action = if d.this {
            pill("rename", t!("Rename"), p.text, cx).on_click(cx.listener(|this, _, window, cx| this.edit_name(true, window, cx)))
        } else {
            pill(SharedString::from(format!("sign-out-{id}")), t!("Sign out"), p.negative, cx)
                .on_click(cx.listener(move |this, _, _, cx| this.sign_out_device(id.clone(), Proof::Ask, cx)))
        };
        let out = self.verify.out.as_ref().filter(|o| o.device == d.id);
        v_flex().child(panels::row(&d.name, &detail, cx).child(action)).children(out.map(|o| self.sign_out_form(o, cx))).into_any_element()
    }

    /// What signing out a device asks for, under its row.
    fn sign_out_form(&self, out: &SignOut, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let text = |s: String| div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(s);
        let mut form = v_flex().pb(px(12.)).gap(px(8.));
        let device = out.device.clone();
        let (line, field, button): (String, bool, Option<(&'static str, Proof)>) = match &out.need {
            None => (t!("Asking the server what it needs to sign the device out.").into(), false, None),
            Some(Auth::Password) => (t!("Enter your password to sign this device out.").into(), true, None),
            Some(Auth::Code { email }) => (t!("Beeper emails a code to {email} to confirm it is you.", email = email), false, Some((t!("Email me a code"), Proof::SendCode))),
            Some(Auth::CodeSent) => (t!("Enter the code from the email.").into(), true, None),
            Some(Auth::Unsupported) => {
                (t!("This server asks for a kind of confirmation Unisend does not offer. Sign the device out from Beeper or Element.").into(), false, None)
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
                    .label(t!("Sign out"))
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
        let close = if out.need == Some(Auth::Unsupported) { t!("Close") } else { t!("Cancel") };
        buttons = buttons
            .child(Button::new("sign-out-cancel").ghost().h(px(TAP)).label(close).on_click(cx.listener(|this, _, window, cx| this.close_sign_out(window, cx))));
        form.child(buttons).children(out.error.clone().map(|e| div().text_size(fs(14.)).text_color(p.negative).child(e)))
    }

    /// Rises like the chat's sheet. A tap outside closes it only once nothing is running, so a
    /// stray tap does not cancel a comparison.
    pub(super) fn render_verify_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.verify.sheet.is_some() {
            self.verify.sheet_last = self.verify.sheet.clone();
        }
        let Some(frame) = self.verify.motion.sample(self.verify.sheet.is_some(), window, cx) else {
            self.verify.sheet_last = None;
            return None;
        };
        let live = frame.live;
        let state = self.verify.sheet_last.clone()?;
        let p = Palette::of(cx);
        let (_, bottom) = crate::insets();
        let (title, body, primary, secondary): (&str, String, Option<Primary>, (&str, Option<Cmd>)) = match &state {
            Verify::Incoming { who } => (
                t!("Verification request"),
                t!("{who} asks to verify. Accept only if you started this yourself.", who = who),
                Some((t!("Accept"), Cmd::VerifyAccept, Some(Verify::Waiting))),
                (t!("Decline"), Some(Cmd::VerifyCancel)),
            ),
            Verify::Waiting => (
                t!("Waiting for the other device"),
                t!("Open Beeper, Element or Unisend where you are signed in and accept the request there.").into(),
                None,
                (t!("Cancel"), Some(Cmd::VerifyCancel)),
            ),
            Verify::Emoji(_) => (
                t!("Compare emoji"),
                t!("Check that the other device shows the same emoji in the same order.").into(),
                Some((t!("They match"), Cmd::VerifyMatch(true), Some(Verify::Confirmed))),
                (t!("They don't match"), Some(Cmd::VerifyMatch(false))),
            ),
            Verify::Confirmed => {
                (t!("Waiting for the other device"), t!("Confirm on the other device that the emoji match.").into(), None, (t!("Cancel"), Some(Cmd::VerifyCancel)))
            }
            Verify::Done => {
                (t!("Verified"), t!("Both sides confirmed the same emoji. Messages this device could not read may now unlock.").into(), None, (t!("Done"), None))
            }
            Verify::Failed(why) => (t!("Not verified"), why.clone(), None, (t!("Close"), None)),
        };
        let settled = matches!(state, Verify::Done | Verify::Failed(_));
        let emoji = match &state {
            Verify::Emoji(list) => Some(h_flex().flex_wrap().justify_center().gap_y(px(12.)).py(px(8.)).children(list.iter().enumerate().map(|(i, (symbol, name))| {
                // One after another, so the eye reads them in order.
                let shown = Presence::new(ElementId::from(("verify-emoji", i)), true).transition(motion::enter().delay(EMOJI_STEP * i as u32)).sample(window, cx).progress;
                v_flex()
                    .relative()
                    .top(px(8. * (1. - shown)))
                    .opacity(shown)
                    .w(px(80.))
                    .items_center()
                    .gap(px(4.))
                    .child(div().text_size(px(32.)).line_height(px(40.)).child(symbol.clone()))
                    .child(div().text_size(fs(12.)).text_color(p.muted).child(name.clone()))
            }))),
            _ => None,
        };
        let key = match &state {
            Verify::Incoming { .. } => 0,
            Verify::Waiting => 1,
            Verify::Emoji(_) => 2,
            Verify::Confirmed => 3,
            Verify::Done => 4,
            Verify::Failed(_) => 5,
        };
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
                        let ended = Verify::Failed(t!("The emoji did not match, so nothing was verified.").into());
                        this.verify_act(Cmd::VerifyMatch(false), Some(ended), cx);
                    }
                    Some(cmd) => this.verify_act(cmd, None, cx),
                    None => this.verify_act_close(cx),
                }
            }))
        }));
        // Dragged down only once nothing is running, as a tap outside.
        let head = div()
            .id("verify-head")
            .h(px(44.))
            .pt(px(8.))
            .flex()
            .items_center()
            .text_size(fs(16.))
            .font_weight(FontWeight::SEMIBOLD)
            .when(live && settled, |el| {
                el.child(sheet::grip(cx, |this: &mut Self| &mut this.verify.motion, |this, cx| this.verify_act_close(cx)))
            })
            .child(title.to_owned());
        let view = cx.entity_id();
        let content = v_flex()
            .child(head)
            .child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(body))
            .children(emoji)
            .child(buttons);
        let content = self.verify.motion.fit("verify-content", key, content, view, window, cx);
        Some(
            sheet::scrim("verify-sheet", frame)
                .when(live, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        if settled {
                            this.verify_act_close(cx);
                        }
                    }))
                })
                .child(
                    v_flex()
                        .id("verify-sheet-body")
                        .relative()
                        .top(px(frame.y))
                        .w_full()
                        .max_w(px(SHEET_W))
                        .px(px(GUTTER + 4.))
                        .pb(px(bottom + 16.))
                        .rounded_t(px(RADIUS))
                        .bg(p.surface)
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(self.verify.motion.probe(view))
                        .child(content),
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
    h_flex().id(id).flex_none().h(px(TAP)).px(px(GAP)).rounded_full().bg(p.raised).text_size(fs(14.)).text_color(ink).cursor_pointer().child(label)
}

/// "Active today", "Active yesterday", "Active on 3 May".
fn seen(ts_ms: u64) -> String {
    let Some(t) = local(ts_ms) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    match (today - t.date_naive()).num_days() {
        ..=0 => t!("Active today").to_owned(),
        1 => t!("Active yesterday").to_owned(),
        _ => t!("Active on {date}", date = crate::i18n::date(&t, t.year() != today.year())),
    }
}
