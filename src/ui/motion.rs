//! How long things take and how they ease, decided once: strong ease-out to enter, exits faster than
//! entrances, springs only where a finger let go, nothing on keyboard actions. GPUI and gpui-kit skip
//! every animation when `App::reduce_motion` is set; `follow_system` sets it from Android.
#![expect(dead_code, reason = "used by the chat-screen and overlay packages; drop when both have landed")]

use std::time::Duration;

use gpui_kit::App;
use gpui_kit::base::{Easing, Spring, Transition};

/// Something small arriving: a message, a reaction, the compose strip, a pointer menu.
pub(crate) const ENTER: Duration = Duration::from_millis(200);
/// Leaving is quicker than arriving.
pub(crate) const EXIT: Duration = Duration::from_millis(150);
/// A chat pushed onto or popped off a phone's screen.
pub(crate) const PUSH: Duration = Duration::from_millis(250);
/// A sheet attached to the bottom edge rising, and falling.
pub(crate) const SHEET_IN: Duration = Duration::from_millis(400);
pub(crate) const SHEET_OUT: Duration = Duration::from_millis(250);
/// The message a quote points at stays tinted this long, then the tint fades.
pub(crate) const FLASH_HOLD: Duration = Duration::from_millis(300);
pub(crate) const FLASH_FADE: Duration = Duration::from_millis(700);
/// The heart a double tap pops at the finger.
pub(crate) const HEART: Duration = Duration::from_millis(700);

/// cubic-bezier(0.23, 1, 0.32, 1): moves at once, settles softly.
pub(crate) const EASE_OUT: Easing = Easing::CubicBezier { x1: 0.23, y1: 1., x2: 0.32, y2: 1. };
/// Vaul's iOS sheet curve, cubic-bezier(0.32, 0.72, 0, 1): steep start, so 400 ms still feels quick.
pub(crate) const EASE_SHEET: Easing = Easing::CubicBezier { x1: 0.32, y1: 0.72, x2: 0., y2: 1. };

pub(crate) fn ease_out(t: f32) -> f32 {
    EASE_OUT.sample(t)
}

pub(crate) fn ease_sheet(t: f32) -> f32 {
    EASE_SHEET.sample(t)
}

pub(crate) fn enter() -> Transition {
    Transition::new(ENTER).easing(EASE_OUT)
}

pub(crate) fn exit() -> Transition {
    Transition::new(EXIT).easing(EASE_OUT)
}

/// Something a finger let go of, returning to rest: quick, with a slight overshoot.
pub(crate) const RELEASE: Spring = Spring::new(Duration::from_millis(300)).with_damping(0.8).with_epsilon(0.5);

#[cfg(target_os = "android")]
pub(crate) fn haptic(feedback: gpui_mobile::packages::vibration::HapticFeedback) {
    if let Err(e) = gpui_mobile::packages::vibration::haptic_feedback(feedback) {
        log::debug!("haptic: {e}");
    }
}

/// Follows Android's "Remove animations" (animator duration scale 0). gpui-kit follows the desktops.
pub(crate) fn follow_system(cx: &mut App) {
    #[cfg(target_os = "android")]
    match animations_off() {
        Ok(off) => cx.set_reduce_motion(off),
        Err(e) => log::debug!("animator scale: {e}"),
    }
    #[cfg(not(target_os = "android"))]
    let _ = cx;
}

#[cfg(target_os = "android")]
fn animations_off() -> Result<bool, String> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;
    helpers::with_env(|env| {
        let activity = helpers::activity(env)?;
        let resolver = env
            .call_method(&activity, jni::jni_str!("getContentResolver"), jni::jni_sig!("()Landroid/content/ContentResolver;"), &[])
            .and_then(|v| v.l())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })?;
        let class = env.find_class(jni::jni_str!("android/provider/Settings$Global")).map_err(|e| {
            env.exception_clear();
            e.to_string()
        })?;
        let key = env.new_string("animator_duration_scale").map_err(|e| e.to_string())?;
        env.call_static_method(
            &class,
            jni::jni_str!("getFloat"),
            jni::jni_sig!("(Landroid/content/ContentResolver;Ljava/lang/String;F)F"),
            &[JValue::Object(&resolver), JValue::Object(&key), JValue::Float(1.)],
        )
        .and_then(|v| v.f())
        .map(|scale| scale == 0.)
        .map_err(|e| {
            env.exception_clear();
            e.to_string()
        })
    })
}
