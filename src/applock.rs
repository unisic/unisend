//! App lock and screen security. On Android `dev.gpui.beeper.AppLock` decides when the app is
//! locked and asks for the phone's lock; the app PIN is checked here (`pin`) and drawn by
//! `ui::pinpad`. Desktop has the app PIN only, asked once when the app starts. Cost: one JNI call
//! per frame while the lock is on, a 250 ms poll only while locked or while the phone's lock is
//! being confirmed.

pub(crate) mod pin;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, v_flex};
use gpui_kit::*;

use crate::Palette;
use crate::i18n::{n_, t};

/// How long the app may stay in the background before it locks again, in seconds.
pub(crate) const TIMEOUTS: [(u32, &str); 4] = [(0, n_!("Immediately")), (60, n_!("1 minute")), (300, n_!("5 minutes")), (1800, n_!("30 minutes"))];

pub(crate) const POLL: Duration = Duration::from_millis(250);

/// How the app is unlocked. The numbers are `AppLock.java`'s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum LockMode {
    #[default]
    Off = 0,
    /// The phone's fingerprint, face, PIN, pattern or password, through the system prompt.
    Phone = 1,
    /// A PIN set in the app, see `pin`.
    Pin = 2,
}

/// Kept in `settings.json`; all off by default.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LockSettings {
    #[serde(default)]
    pub mode: LockMode,
    #[serde(default)]
    pub timeout: u32,
    /// Block screenshots and blank the app in the recents list.
    #[serde(default)]
    pub secure: bool,
}

impl LockSettings {
    /// A timeout that is not one of the choices (an edited file) counts as the nearest shorter one.
    pub fn timeout_secs(&self) -> u32 {
        TIMEOUTS.iter().rev().map(|&(s, _)| s).find(|&s| s <= self.timeout).unwrap_or(0)
    }
}

/// What a change in the settings asks of the user, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The current app PIN.
    CheckPin,
    /// The phone's lock: to prove it is the owner, or that the phone's lock works before it guards the app.
    CheckPhone,
    /// A new app PIN, typed twice.
    NewPin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Goal {
    Mode(LockMode),
    ChangePin,
}

impl Goal {
    pub fn mode(self) -> LockMode {
        match self {
            Goal::Mode(mode) => mode,
            Goal::ChangePin => LockMode::Pin,
        }
    }
}

/// Leaving a mode asks for its own credential first; the phone's lock is tried once before it is
/// relied on, and a new PIN is typed twice.
pub(crate) fn steps(from: LockMode, goal: Goal) -> Vec<Step> {
    if goal == Goal::Mode(from) {
        return Vec::new();
    }
    let mut steps = Vec::with_capacity(2);
    match from {
        LockMode::Off => {}
        LockMode::Phone => steps.push(Step::CheckPhone),
        LockMode::Pin => steps.push(Step::CheckPin),
    }
    match goal.mode() {
        LockMode::Off => {}
        LockMode::Phone if from != LockMode::Phone => steps.push(Step::CheckPhone),
        LockMode::Phone => {}
        LockMode::Pin => steps.push(Step::NewPin),
    }
    steps
}

/// The phone's lock asked for by `confirm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "android"), allow(dead_code, reason = "desktop has no phone lock to ask"))]
pub(crate) enum Confirm {
    Waiting,
    Passed,
    Failed,
}

/// Logs a JNI failure the first time; the per-frame callers would flood the log otherwise.
#[cfg(target_os = "android")]
fn warn_once(once: &AtomicBool, what: &str, e: &str) {
    if !once.swap(true, Ordering::Relaxed) {
        log::warn!("app lock: {what}: {e}");
    }
}

#[cfg(target_os = "android")]
mod jni_calls {
    use std::sync::atomic::AtomicBool;

    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    use super::{Confirm, LockMode, warn_once};
    use crate::java_error;

    const CLASS: &str = "dev.gpui.beeper.AppLock";

    /// Fails open: a broken bridge must not lock the user out of the app for good.
    pub fn is_locked() -> bool {
        static ONCE: AtomicBool = AtomicBool::new(false);
        helpers::with_env(|env| {
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("isLocked"), jni::jni_sig!("()Z"), &[]).and_then(|v| v.z()).map_err(|e| java_error(env, e))
        })
        .unwrap_or_else(|e| {
            warn_once(&ONCE, "isLocked", &e);
            false
        })
    }

    pub fn available() -> bool {
        static ONCE: AtomicBool = AtomicBool::new(false);
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("available"), jni::jni_sig!("(Landroid/app/Activity;)Z"), &[JValue::Object(&activity)])
                .and_then(|v| v.z())
                .map_err(|e| java_error(env, e))
        })
        .unwrap_or_else(|e| {
            warn_once(&ONCE, "available", &e);
            false
        })
    }

    /// `name(Activity)`, for the calls that answer nothing.
    fn call(name: &'static str) {
        let r = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let name = jni::strings::JNIString::from(name);
            env.call_static_method(&class, &name, jni::jni_sig!("(Landroid/app/Activity;)V"), &[JValue::Object(&activity)]).map(|_| ()).map_err(|e| java_error(env, e))
        });
        if let Err(e) = r {
            log::warn!("app lock: {name}: {e}");
        }
    }

    pub fn authenticate() {
        call("authenticate");
    }

    pub fn unlock() {
        call("unlock");
    }

    pub fn confirm() {
        call("confirm");
    }

    /// A failed bridge reads as a failed check, so the settings do not wait for ever.
    pub fn take_confirm() -> Confirm {
        static ONCE: AtomicBool = AtomicBool::new(false);
        let answer = helpers::with_env(|env| {
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("takeConfirm"), jni::jni_sig!("()I"), &[]).and_then(|v| v.i()).map_err(|e| java_error(env, e))
        });
        match answer {
            Ok(1) => Confirm::Waiting,
            Ok(2) => Confirm::Passed,
            Ok(_) => Confirm::Failed,
            Err(e) => {
                warn_once(&ONCE, "takeConfirm", &e);
                Confirm::Failed
            }
        }
    }

    pub fn configure(mode: LockMode, seconds: u32, secure: bool) {
        let r = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(
                &class,
                jni::jni_str!("configure"),
                jni::jni_sig!("(Landroid/app/Activity;IIZ)V"),
                &[JValue::Object(&activity), JValue::Int(mode as i32), JValue::Int(i32::try_from(seconds).unwrap_or(i32::MAX)), JValue::Bool(secure)],
            )
            .map(|_| ())
            .map_err(|e| java_error(env, e))
        });
        if let Err(e) = r {
            log::warn!("app lock: configure: {e}");
        }
    }
}

/// Desktop has no phone lock and no "leaving the app" to time: the app PIN is asked once, at start.
#[cfg(not(target_os = "android"))]
mod jni_calls {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Confirm, LockMode};

    static LOCKED: AtomicBool = AtomicBool::new(true);

    pub fn is_locked() -> bool {
        LOCKED.load(Ordering::Relaxed)
    }
    pub fn available() -> bool {
        false
    }
    pub fn authenticate() {}
    pub fn unlock() {
        LOCKED.store(false, Ordering::Relaxed);
    }
    pub fn confirm() {}
    pub fn take_confirm() -> Confirm {
        Confirm::Failed
    }
    pub fn configure(mode: LockMode, _: u32, _: bool) {
        if mode == LockMode::Off {
            LOCKED.store(false, Ordering::Relaxed);
        }
    }
}

static WATCHING: AtomicBool = AtomicBool::new(false);

/// Redraws once the credential has been accepted. One loop at most, it ends at the unlock.
pub(crate) fn watch<T: 'static>(cx: &mut Context<T>) {
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            if !is_locked() {
                break;
            }
        }
        WATCHING.store(false, Ordering::Release);
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

pub(crate) use jni_calls::{authenticate, available, configure, confirm, is_locked, take_confirm, unlock};

/// The lock screen: opaque ground, the app's name, and the button that asks again.
pub(crate) fn screen(cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    v_flex()
        .id("app-lock")
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(16.))
        .bg(p.ground)
        .text_color(p.text)
        .child(Icon::new(IconName::Lock).size_8().text_color(p.muted))
        .child(div().text_size(px(18.)).child(t!("Unisend is locked")))
        .child(
            div()
                .id("app-lock-unlock")
                .h(px(48.))
                .px(px(24.))
                .flex()
                .items_center()
                .rounded_full()
                .bg(p.raised)
                .text_size(px(16.))
                .cursor_pointer()
                .on_click(|_, _, _| authenticate())
                .child(t!("Unlock")),
        )
}

#[cfg(test)]
mod tests {
    use super::{Goal, LockMode, LockSettings, Step, steps};

    #[test]
    fn odd_timeouts_round_down() {
        let t = |timeout| LockSettings { timeout, ..Default::default() }.timeout_secs();
        assert_eq!((t(0), t(59), t(60), t(299), t(300), t(99999)), (0, 0, 60, 60, 300, 1800));
    }

    #[test]
    fn old_settings_file_has_lock_off() {
        let s: crate::Settings = serde_json::from_str(r#"{"theme":"Dark"}"#).unwrap();
        assert_eq!(s.lock, LockSettings::default());
    }

    #[test]
    fn settings_keep_the_mode() {
        let s = LockSettings { mode: LockMode::Pin, timeout: 60, secure: true };
        let back: LockSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn leaving_a_mode_asks_for_its_credential() {
        use LockMode::*;
        use Step::*;
        for mode in [Off, Phone, Pin] {
            assert!(steps(mode, Goal::Mode(mode)).is_empty());
        }
        assert_eq!(steps(Off, Goal::Mode(Pin)), [NewPin]);
        assert_eq!(steps(Off, Goal::Mode(Phone)), [CheckPhone]);
        assert_eq!(steps(Phone, Goal::Mode(Off)), [CheckPhone]);
        assert_eq!(steps(Phone, Goal::Mode(Pin)), [CheckPhone, NewPin]);
        assert_eq!(steps(Pin, Goal::Mode(Off)), [CheckPin]);
        assert_eq!(steps(Pin, Goal::Mode(Phone)), [CheckPin, CheckPhone]);
        assert_eq!(steps(Pin, Goal::ChangePin), [CheckPin, NewPin]);
        assert_eq!(Goal::ChangePin.mode(), Pin);
    }
}
