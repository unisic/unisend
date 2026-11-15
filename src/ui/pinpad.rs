//! The app PIN's keypad: the lock screen in App PIN mode, and the steps of a change to the app
//! lock in the settings (the current PIN, a new one twice, the phone's lock).

use super::*;
use crate::applock::pin::{self, Check};
use crate::applock::{self, Confirm, Goal, LockMode, Step};

/// A key's side: three in a row fit a 320 px wide phone with room between them.
const KEY: f32 = 72.;
const SHAKE: Duration = Duration::from_millis(400);

#[derive(Clone, Copy)]
enum Key {
    Digit(char),
    Back,
    Done,
}

#[derive(Default)]
pub(super) struct Pad {
    entry: String,
    /// A new PIN's first entry, while it is typed again.
    first: Option<String>,
    note: Option<SharedString>,
    /// The last wrong PIN, for the dots' shake.
    shake: Option<Instant>,
    /// A PIN is being hashed off the UI thread.
    busy: bool,
    /// Digits the dots keep showing while that PIN is checked and while a wrong one shakes.
    shown: usize,
    /// Unix seconds before which no PIN is tried.
    retry_at: u64,
    /// `retry_at` has been read from the file for this screen.
    read: bool,
    /// The one countdown loop runs.
    ticking: bool,
    /// A change in the settings, with its steps still to do, the current one first.
    flow: Option<(Goal, Vec<Step>)>,
}

/// Horizontal offset of the dots at `t` in 0..1 of a shake: three swings that die out.
fn shake_offset(t: f32) -> f32 {
    (t * std::f32::consts::PI * 6.).sin() * 10. * (1. - t)
}

fn wait_text(secs: u64) -> SharedString {
    if secs >= 60 {
        tn!(secs.div_ceil(60), "Too many wrong tries. Try again in {n} minute.", "Too many wrong tries. Try again in {n} minutes.").into()
    } else {
        tn!(secs, "Too many wrong tries. Try again in {n} second.", "Too many wrong tries. Try again in {n} seconds.").into()
    }
}

impl BeeperApp {
    fn pin_locked(&self) -> bool {
        self.settings.lock.mode == LockMode::Pin && applock::is_locked()
    }

    fn pad_step(&self) -> Option<Step> {
        self.pad.flow.as_ref().and_then(|(_, steps)| steps.first().copied())
    }

    /// The settings show the keypad instead of their list.
    pub(super) fn pad_page(&self) -> bool {
        matches!(self.pad_step(), Some(Step::CheckPin | Step::NewPin))
    }

    fn pad_wait(&self) -> u64 {
        self.pad.retry_at.saturating_sub(pin::now())
    }

    /// Starts what the settings ask for; nothing when it is the mode already on.
    pub(super) fn change_lock(&mut self, goal: Goal, cx: &mut Context<Self>) {
        let from = self.settings.lock.mode;
        if goal == Goal::Mode(from) {
            return;
        }
        let mut steps = applock::steps(from, goal);
        // A phone lock that was removed lets anyone in already; asking for it would only trap the setting.
        if from == LockMode::Phone && !applock::available() {
            steps.retain(|&step| step != Step::CheckPhone);
        }
        self.pad = Pad { flow: Some((goal, steps)), ..Default::default() };
        self.begin_step(cx);
    }

    /// Leaves a change half done, with nothing saved. True when there was one.
    pub(super) fn cancel_change(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pad.flow.is_none() {
            return false;
        }
        self.pad = Pad::default();
        cx.notify();
        true
    }

    fn begin_step(&mut self, cx: &mut Context<Self>) {
        (self.pad.first, self.pad.read) = (None, false);
        self.pad.entry.clear();
        match self.pad_step() {
            None => self.finish_change(cx),
            Some(Step::CheckPhone) => {
                applock::confirm();
                self.poll_confirm(cx);
            }
            Some(_) => {}
        }
        cx.notify();
    }

    fn next_step(&mut self, cx: &mut Context<Self>) {
        if let Some((_, steps)) = &mut self.pad.flow
            && !steps.is_empty()
        {
            steps.remove(0);
        }
        self.begin_step(cx);
    }

    fn finish_change(&mut self, cx: &mut Context<Self>) {
        let Some((goal, _)) = self.pad.flow.take() else {
            return;
        };
        self.pad = Pad::default();
        let mode = goal.mode();
        if mode != LockMode::Pin {
            pin::clear(&self.dir);
        }
        self.set_lock(Some(mode), None, None, cx);
    }

    /// Waits for the system prompt's answer; ends with it, or when the change is left.
    fn poll_confirm(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(applock::POLL).await;
                let answer = applock::take_confirm();
                let go_on = this
                    .update(cx, |this, cx| {
                        if this.pad_step() != Some(Step::CheckPhone) {
                            return false;
                        }
                        match answer {
                            Confirm::Waiting => return true,
                            Confirm::Passed => this.next_step(cx),
                            Confirm::Failed => {
                                this.cancel_change(cx);
                            }
                        }
                        false
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
    }

    /// One redraw a second while a wait is shown, to count it down; at most an hour.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.pad.ticking || self.pad_wait() == 0 {
            return;
        }
        self.pad.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let more = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let more = this.pad_wait() > 0;
                        this.pad.ticking = more;
                        more
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    /// Digits, Backspace and Enter from a keyboard, while a keypad is shown.
    pub(super) fn pad_keystroke(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if !self.pin_locked() && !self.pad_page() {
            return false;
        }
        let key = match key {
            "backspace" => Key::Back,
            "enter" => Key::Done,
            _ => match key.chars().collect::<Vec<_>>()[..] {
                [d] if d.is_ascii_digit() => Key::Digit(d),
                _ => return false,
            },
        };
        self.press(key, cx);
        true
    }

    fn press(&mut self, key: Key, cx: &mut Context<Self>) {
        if self.pad.busy || self.pad_wait() > 0 {
            return;
        }
        match key {
            Key::Digit(d) if self.pad.entry.len() < pin::MAX_LEN => {
                self.pad.entry.push(d);
                self.pad.note = None;
                #[cfg(target_os = "android")]
                motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Selection);
            }
            Key::Digit(_) => {}
            Key::Back => {
                self.pad.entry.pop();
            }
            Key::Done => self.submit_pin(cx),
        }
        cx.notify();
    }

    fn submit_pin(&mut self, cx: &mut Context<Self>) {
        if self.pad.entry.len() < pin::MIN_LEN {
            return;
        }
        self.pad.shown = self.pad.entry.len();
        let entry = std::mem::take(&mut self.pad.entry);
        let dir = self.dir.clone();
        if self.pin_locked() || self.pad_step() == Some(Step::CheckPin) {
            self.pad.busy = true;
            cx.spawn(async move |this, cx| {
                let result = cx.background_spawn(async move { pin::check(&dir, &entry, pin::now()) }).await;
                let _ = this.update(cx, |this, cx| this.checked(result, cx));
            })
            .detach();
            return;
        }
        match self.pad.first.take() {
            None if !pin::valid(&entry) => self.pad.note = Some(t!("A PIN has 4 to 8 digits.").into()),
            None => self.pad.first = Some(entry),
            Some(first) if first == entry => {
                self.pad.busy = true;
                cx.spawn(async move |this, cx| {
                    let saved = cx.background_spawn(async move { pin::set(&dir, &entry) }).await;
                    let _ = this.update(cx, |this, cx| {
                        this.pad.busy = false;
                        match saved {
                            Ok(()) => this.next_step(cx),
                            Err(e) => {
                                log::warn!("app PIN: {e}");
                                this.pad.note = Some(t!("Could not save the PIN.").into());
                                cx.notify();
                            }
                        }
                    });
                })
                .detach();
            }
            Some(_) => {
                self.pad.note = Some(t!("The PINs did not match. Choose a PIN again.").into());
                self.wrong();
            }
        }
    }

    fn checked(&mut self, result: Check, cx: &mut Context<Self>) {
        self.pad.busy = false;
        match result {
            Check::Right | Check::Missing => {
                if result == Check::Missing {
                    // Only a broken or deleted file gets here; there is nothing to check against.
                    log::warn!("app lock: no app PIN stored, the lock is turned off");
                    self.set_lock(Some(LockMode::Off), None, None, cx);
                }
                if self.pin_locked() || result == Check::Missing {
                    applock::unlock();
                    self.pad = Pad::default();
                } else if self.pad_step() == Some(Step::CheckPin) {
                    self.next_step(cx);
                }
            }
            Check::Wrong(wait) => {
                self.pad.retry_at = pin::now() + wait;
                self.pad.note = Some(t!("Wrong PIN.").into());
                self.wrong();
                self.tick(cx);
            }
            Check::Wait(left) => {
                self.pad.retry_at = pin::now() + left;
                self.tick(cx);
            }
        }
        cx.notify();
    }

    fn wrong(&mut self) {
        self.pad.shake = Some(Instant::now());
        #[cfg(target_os = "android")]
        motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Heavy);
    }

    /// The wait left from the file, once per screen: killing the app does not end it.
    fn read_wait(&mut self, cx: &mut Context<Self>) {
        if !self.pad.read {
            self.pad.read = true;
            let now = pin::now();
            self.pad.retry_at = now + pin::wait(&self.dir, now);
            self.tick(cx);
        }
    }

    /// The whole window while the app is locked: the phone's prompt behind a button, or the keypad.
    pub(super) fn render_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.pad.flow.is_some() {
            self.pad = Pad::default();
        }
        applock::watch(cx);
        if self.settings.lock.mode != LockMode::Pin {
            return applock::screen(cx).into_any_element();
        }
        self.read_wait(cx);
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        v_flex()
            .id("app-lock")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.pad_keystroke(&event.keystroke.key, cx) {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .pt(px(top + 24.))
            .pb(px(bottom + 24.))
            .items_center()
            .justify_center()
            .gap(px(GAP))
            .font_family(crate::FACE)
            .bg(p.ground)
            .text_color(p.text)
            .child(Icon::new(IconName::Lock).size_6().text_color(p.muted))
            .child(self.render_pad(t!("Enter your Unisend PIN"), None, false, window, cx))
            .into_any_element()
    }

    /// The settings' page for the step that takes a PIN.
    pub(super) fn render_lock_change(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let forgot = t!("If you forget it, the only way back in is to clear Unisend's data and sign in again.");
        let (prompt, checking) = match (self.pad_step(), &self.pad.first) {
            (Some(Step::CheckPin), _) => {
                self.read_wait(cx);
                (t!("Enter your current PIN"), true)
            }
            (_, None) => (t!("Choose a PIN of 4 to 8 digits"), false),
            (_, Some(_)) => (t!("Enter the new PIN again"), false),
        };
        // The hint keeps its space while checking, so the keys stay put from one step to the next.
        div().w_full().pt(px(16.)).flex().justify_center().child(self.render_pad(prompt, Some(forgot), checking, window, cx)).into_any_element()
    }

    fn render_pad(&mut self, prompt: &'static str, hint: Option<&'static str>, hide_hint: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let wait = self.pad_wait();
        let ready = self.pad.entry.len() >= pin::MIN_LEN && !self.pad.busy && wait == 0;
        let (offset, shaking) = match self.pad.shake {
            Some(at) if at.elapsed() < SHAKE && !cx.reduce_motion() => {
                window.request_animation_frame();
                (shake_offset(at.elapsed().as_secs_f32() / SHAKE.as_secs_f32()), true)
            }
            _ => {
                self.pad.shake = None;
                (0., false)
            }
        };
        let len = if self.pad.entry.is_empty() && (self.pad.busy || shaking) { self.pad.shown } else { self.pad.entry.len() };
        let note = if wait > 0 { Some(wait_text(wait)) } else { self.pad.note.clone() };
        let dots = h_flex().h(px(24.)).gap(px(14.)).relative().left(px(offset)).children((0..len.max(pin::MIN_LEN)).map(|i| {
            let dot = div().size(px(14.)).rounded_full();
            if i < len { dot.bg(p.text) } else { dot.border_2().border_color(p.faint) }
        }));
        let key = |id: &'static str, key: Key, tint: Hsla, window: &mut Window, cx: &mut Context<Self>| {
            let press = press::area(id, Duration::ZERO, tint, window, cx).rounded_full();
            div()
                .id(id)
                .relative()
                .child(press)
                .size(px(KEY))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.press(key, cx)))
        };
        let digit = |d: char, window: &mut Window, cx: &mut Context<Self>| {
            const IDS: [&str; 10] = ["pin-0", "pin-1", "pin-2", "pin-3", "pin-4", "pin-5", "pin-6", "pin-7", "pin-8", "pin-9"];
            key(IDS[d as usize - '0' as usize], Key::Digit(d), p.raised, window, cx).bg(p.surface).text_size(fs(28.)).child(d.to_string())
        };
        let mut keys = v_flex().gap(px(12.));
        for row in [['1', '2', '3'], ['4', '5', '6'], ['7', '8', '9']] {
            keys = keys.child(h_flex().gap(px(20.)).children(row.map(|d| digit(d, window, cx))));
        }
        let done = key("pin-done", Key::Done, if ready { p.muted } else { p.raised }, window, cx)
            .when(ready, |el| el.bg(p.text).text_color(p.ground))
            .when(!ready, |el| el.bg(p.surface).text_color(p.faint))
            .child(Icon::new(IconName::Check).size_6());
        let back = key("pin-back", Key::Back, p.surface, window, cx).text_color(p.muted).child(Icon::new(IconName::Delete).size_6());
        keys = keys.child(h_flex().gap(px(20.)).child(back).child(digit('0', window, cx)).child(done));
        v_flex()
            .items_center()
            .max_w(px(360.))
            .px(px(GUTTER))
            .gap(px(GAP))
            .child(div().text_size(fs(18.)).line_height(fs(24.)).text_center().child(prompt))
            .when_some(hint, |el, hint| el.child(div().text_size(fs(13.)).line_height(fs(18.)).text_center().text_color(p.muted).when(hide_hint, |el| el.invisible()).child(hint)))
            .child(dots)
            .child(div().min_h(px(40.)).text_size(fs(14.)).line_height(fs(20.)).text_center().text_color(p.negative).children(note))
            .child(keys.opacity(if wait > 0 { 0.4 } else { 1. }))
    }
}

#[cfg(test)]
mod tests {
    use super::{shake_offset, wait_text};

    #[test]
    fn shake_starts_and_ends_at_rest() {
        assert_eq!(shake_offset(0.), 0.);
        assert!(shake_offset(0.999).abs() < 0.1);
        assert!(shake_offset(1. / 12.).abs() > 8.);
    }

    #[test]
    fn waits_read_in_seconds_then_whole_minutes() {
        // Holds in English and in Polish, which another test may switch on.
        assert!(wait_text(30).contains("30 se"));
        assert!(wait_text(61).contains("2 min"));
        assert!(wait_text(3600).contains("60 min"));
    }
}
