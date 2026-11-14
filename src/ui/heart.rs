//! Double tap on a message: a red heart, as in Instagram and Messenger.

use super::*;

/// The heart is this wide, in logical pixels, at full size.
const SIZE: f32 = 56.;

/// Scale and opacity of the pop at `t` in 0..1: it springs up past full size, settles, then fades.
fn frame(t: f32) -> (f32, f32) {
    const RISE: f32 = 0.3;
    const FADE_FROM: f32 = 0.6;
    let scale = if t < RISE { 0.4 + 0.8 * motion::ease_out(t / RISE) } else { 1.2 - 0.2 * motion::ease_out(((t - RISE) / 0.2).min(1.)) };
    let opacity = if t < FADE_FROM { 1. } else { 1. - (t - FADE_FROM) / (1. - FADE_FROM) };
    (scale, opacity)
}

impl BeeperApp {
    /// A double tap at `at` on message `id`: gives the heart, or takes ours back.
    pub(super) fn double_tap(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        let ours = self.msgs.iter().any(|m| m.id == id && m.reactions.iter().any(|r| r.own && menu::same_emoji(&r.key, LIKE)));
        self.toggle_reaction(id, LIKE, cx);
        if !ours {
            self.heart_pop = Some((at, Instant::now()));
        }
        #[cfg(target_os = "android")]
        motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
    }

    /// The heart over the tap point while it lasts; nothing under reduced motion.
    pub(super) fn render_heart(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, since) = self.heart_pop?;
        let t = since.elapsed().as_secs_f32() / motion::HEART.as_secs_f32();
        if t >= 1. || cx.reduce_motion() {
            self.heart_pop = None;
            return None;
        }
        window.request_animation_frame();
        let (scale, opacity) = frame(t);
        Some(
            div()
                .absolute()
                .left(at.x - px(SIZE / 2.))
                .top(at.y - px(SIZE / 2.))
                .size(px(SIZE))
                .flex()
                .items_center()
                .justify_center()
                .opacity(opacity)
                .text_size(fs(SIZE * scale))
                .line_height(fs(SIZE))
                .child(LIKE)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::frame;

    #[test]
    fn pop_grows_then_fades_out() {
        let (start, _) = frame(0.);
        let (peak, _) = frame(0.3);
        assert!(start < 0.5 && peak > 1.1);
        assert_eq!(frame(0.5).1, 1.);
        assert!(frame(0.999).1 < 0.01);
    }
}
