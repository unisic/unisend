//! Screen lock and screen security (Android only). `dev.gpui.beeper.AppLock` decides when the app
//! is locked and asks for the credential; this side keeps the settings and draws the lock screen
//! while `is_locked()`. Cost: one JNI call per frame while the lock is on, a 250 ms poll only
//! while locked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, v_flex};
use gpui_kit::*;

use crate::Palette;

/// How long the app may stay in the background before it locks again, in seconds.
pub(crate) const TIMEOUTS: [(u32, &str); 4] = [(0, "Immediately"), (60, "1 minute"), (300, "5 minutes"), (1800, "30 minutes")];

pub(crate) const POLL: Duration = Duration::from_millis(250);

/// Kept in `settings.json`; both off by default.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LockSettings {
    /// Ask for the device's biometric or credential when the app is opened.
    #[serde(default)]
    pub lock: bool,
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

#[cfg(target_os = "android")]
mod jni_calls {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    const CLASS: &str = "dev.gpui.beeper.AppLock";

    pub fn is_locked() -> bool {
        helpers::with_env(|env| {
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("isLocked"), jni::jni_sig!("()Z"), &[]).and_then(|v| v.z()).map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        })
        .unwrap_or(false)
    }

    pub fn available() -> bool {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(
                &class,
                jni::jni_str!("available"),
                jni::jni_sig!("(Landroid/app/Activity;)Z"),
                &[JValue::Object(&activity)],
            )
            .and_then(|v| v.z())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        })
        .unwrap_or(false)
    }

    pub fn authenticate() {
        let r = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(
                &class,
                jni::jni_str!("authenticate"),
                jni::jni_sig!("(Landroid/app/Activity;)V"),
                &[JValue::Object(&activity)],
            )
            .map(|_| ())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        });
        if let Err(e) = r {
            log::warn!("app lock: {e}");
        }
    }

    pub fn configure(lock: bool, seconds: u32, secure: bool) {
        let r = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(
                &class,
                jni::jni_str!("configure"),
                jni::jni_sig!("(Landroid/app/Activity;ZIZ)V"),
                &[JValue::Object(&activity), JValue::Bool(lock), JValue::Int(i32::try_from(seconds).unwrap_or(i32::MAX)), JValue::Bool(secure)],
            )
            .map(|_| ())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        });
        if let Err(e) = r {
            log::warn!("app lock: {e}");
        }
    }
}

#[cfg(not(target_os = "android"))]
mod jni_calls {
    pub fn is_locked() -> bool {
        false
    }
    pub fn available() -> bool {
        false
    }
    pub fn authenticate() {}
    pub fn configure(_: bool, _: u32, _: bool) {}
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

pub(crate) use jni_calls::{authenticate, available, configure, is_locked};

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
        .child(div().text_size(px(18.)).child("Murmur is locked"))
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
                .child("Unlock"),
        )
}

#[cfg(test)]
mod tests {
    use super::LockSettings;

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
}
