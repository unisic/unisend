//! How long things take and how they ease, decided once: strong ease-out to enter, exits faster than
//! entrances, springs only where a finger let go, nothing on keyboard actions. GPUI and gpui-kit skip
//! every animation when `App::reduce_motion` is set; `follow_system` sets it from Android.

use std::time::{Duration, Instant};

use gpui_kit::App;
use gpui_kit::base::{Easing, Spring, Transition};
use gpui_kit::gpui::{SpringConfig, SpringState};

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
// Sheets
/// A sheet leaving under the one that replaces it, and how long the new one waits to rise.
pub(crate) const SHEET_HANDOFF: Duration = Duration::from_millis(200);
pub(crate) const HANDOFF_DELAY: Duration = Duration::from_millis(60);
/// New content inside a sheet, a thumbnail that finished loading.
pub(crate) const FADE_IN: Duration = Duration::from_millis(150);

// Navigation and the chat list.
/// A chat or a deep panel sliding in; the way back is `PUSH`.
pub(crate) const PUSH_IN: Duration = Duration::from_millis(300);
/// How far the screen underneath retreats, as a share of the width, and how dark its veil gets.
pub(crate) const PUSH_PARALLAX: f32 = 0.25;
pub(crate) const PUSH_VEIL: f32 = 0.5;
/// The crossfade that stands in for the slide when motion is reduced.
pub(crate) const PUSH_FADE: Duration = Duration::from_millis(150);
/// The chat list scrolling back to its top.
pub(crate) const SCROLL_TOP: Duration = Duration::from_millis(300);
/// The row that was left fades back from `raised`.
pub(crate) const ROW_FADE: Duration = Duration::from_millis(300);
/// Rows displaced by a new message settle with this spring (no finger, so no velocity).
pub(crate) const ROW_SETTLE: (Duration, f32) = (Duration::from_millis(350), 1.0);

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
            .map_err(|e| crate::java_error(env, e))?;
        let class = env.find_class(jni::jni_str!("android/provider/Settings$Global")).map_err(|e| crate::java_error(env, e))?;
        let key = env.new_string("animator_duration_scale").map_err(|e| e.to_string())?;
        env.call_static_method(
            &class,
            jni::jni_str!("getFloat"),
            jni::jni_sig!("(Landroid/content/ContentResolver;Ljava/lang/String;F)F"),
            &[JValue::Object(&resolver), JValue::Object(&key), JValue::Float(1.)],
        )
        .and_then(|v| v.f())
        .map(|scale| scale == 0.)
        .map_err(|e| crate::java_error(env, e))
    })
}

/// Lands a page or a sheet against an edge: 300 ms, critically damped, no overshoot.
#[allow(dead_code, reason = "used by the motion wave that lands next")]
pub(crate) const SETTLE: (Duration, f32) = (Duration::from_millis(300), 1.0);
/// Throws something off screen at the finger's speed: 350 ms, critically damped.
#[allow(dead_code, reason = "used by the motion wave that lands next")]
pub(crate) const THROW: (Duration, f32) = (Duration::from_millis(350), 1.0);
// Photo viewer and media bubbles.
/// A picture flying from its bubble to full screen, and back into it.
pub(crate) const HERO_IN: Duration = Duration::from_millis(260);
pub(crate) const HERO_OUT: Duration = Duration::from_millis(200);
/// The full picture fading in over the thumbnail that stood in for it.
pub(crate) const SHARPEN: Duration = Duration::from_millis(180);
/// A thumbnail fading into its bubble once it has loaded.
pub(crate) const THUMB_IN: Duration = Duration::from_millis(150);
/// A picture pulled up or down returning to rest: like `RELEASE`, with a slight overshoot.
pub(crate) const RETURN: (Duration, f32) = (Duration::from_millis(300), 0.8);

/// A spring the app steps itself, so a released finger's speed carries into it.
/// Call `step` and `window.request_animation_frame()` only during render: requesting a frame elsewhere panicked.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code, reason = "used by the motion wave that lands next")]
pub(crate) struct Glide {
    pub pos: f32,
    pub vel: f32,
    target: f32,
    config: SpringConfig,
    epsilon: f32,
    last: Option<Instant>,
}

#[allow(dead_code, reason = "used by the motion wave that lands next")]
impl Glide {
    /// response = period of the undamped spring, damping = ratio (1.0 critical). Mass 1: stiffness (2π/response)², damping coefficient 4π·ratio/response.
    pub(crate) fn new(response: Duration, damping: f32, epsilon: f32, pos: f32) -> Self {
        let w = std::f32::consts::TAU / response.as_secs_f32();
        Self { pos, vel: 0.0, target: pos, config: SpringConfig::new(w * w, 2.0 * w * damping, 1.0), epsilon, last: None }
    }

    /// The finger holds it: follows `pos`, no velocity, not running.
    pub(crate) fn hold(&mut self, pos: f32) {
        self.set(pos);
    }

    /// The finger let go at `vel` px/s (clamped to ±8000); springs toward `target` from the current pos.
    pub(crate) fn release(&mut self, target: f32, vel: f32, now: Instant) {
        self.target = target;
        self.vel = vel.clamp(-8000.0, 8000.0);
        self.last = Some(now);
    }

    /// Retarget while running, keeping the current velocity (interruptible).
    pub(crate) fn retarget(&mut self, target: f32) {
        self.target = target;
    }

    /// Advances to `now` (delta capped at 50 ms so a stalled frame does not jump) and returns pos. Snaps to the target and stops once |pos-target| < epsilon and |vel| < epsilon*10.
    pub(crate) fn step(&mut self, now: Instant) -> f32 {
        let Some(last) = self.last else { return self.pos };
        let dt = now.saturating_duration_since(last).min(Duration::from_millis(50)).as_secs_f32();
        let next = self.config.step(SpringState { position: self.pos, velocity: self.vel }, self.target, dt);
        self.pos = next.position;
        self.vel = next.velocity;
        if (self.pos - self.target).abs() < self.epsilon && self.vel.abs() < self.epsilon * 10.0 {
            self.set(self.target);
        } else {
            self.last = Some(now);
        }
        self.pos
    }

    /// True while it still needs frames.
    pub(crate) fn running(&self) -> bool {
        self.last.is_some()
    }

    /// Jumps to `pos` and stops (reduce motion, or a value set without animation).
    pub(crate) fn set(&mut self, pos: f32) {
        self.pos = pos;
        self.target = pos;
        self.vel = 0.0;
        self.last = None;
    }

    pub(crate) fn target(&self) -> f32 {
        self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: Duration = Duration::from_millis(8);

    fn glide() -> Glide {
        Glide::new(SETTLE.0, SETTLE.1, 0.5, 0.0)
    }

    #[test]
    fn critical_glide_settles_without_overshoot() {
        let t0 = Instant::now();
        let mut g = glide();
        g.release(100.0, 0.0, t0);
        for i in 1..=125 {
            assert!(g.step(t0 + DT * i) <= 100.0);
        }
        assert!(!g.running());
        assert_eq!(g.pos, 100.0);
    }

    #[test]
    fn release_carries_velocity() {
        let t0 = Instant::now();
        let mut g = glide();
        g.release(100.0, 2000.0, t0);
        let moved = g.step(t0 + DT);
        assert!((moved - 16.0).abs() < 16.0 * 0.3, "moved {moved}");
    }

    #[test]
    fn stalled_frame_is_capped() {
        let t0 = Instant::now();
        let (mut a, mut b) = (glide(), glide());
        a.release(100.0, 0.0, t0);
        b.release(100.0, 0.0, t0);
        assert_eq!(a.step(t0 + Duration::from_millis(500)), b.step(t0 + Duration::from_millis(50)));
    }

    #[test]
    fn hold_stops() {
        let t0 = Instant::now();
        let mut g = glide();
        g.release(100.0, 500.0, t0);
        assert!(g.running());
        g.hold(10.0);
        assert!(!g.running());
        assert_eq!((g.pos, g.vel), (10.0, 0.0));
    }

    #[test]
    fn retarget_keeps_velocity_sign() {
        let t0 = Instant::now();
        let mut g = glide();
        g.release(100.0, 2000.0, t0);
        g.step(t0 + DT);
        g.retarget(0.0);
        assert_eq!(g.target(), 0.0);
        assert!(g.vel > 0.0);
    }
}
