//! A phone's screens stacked: the list underneath, a chat or a deep panel pushed over it, the chat
//! following Android's back gesture as an iOS screen follows the swipe back, and the chat list's
//! rows settling when a new message reorders them.

use std::collections::HashMap;

use super::motion::{self, Glide};
use super::*;

/// Most rows remembered to see which of them moved.
const TRACKED: usize = 30;
/// More rows than this changing in one batch is a sync, not news.
const MAX_CHANGED: usize = 3;

#[derive(Default)]
pub(super) struct Slide {
    /// A pop is running: the chat stays drawn, takes no touches, and closes when it lands.
    pub leaving: Option<Instant>,
    /// The next frame adopts the position without travelling (a notification, a keyboard).
    pub jump: bool,
    /// The deep panel last shown, kept drawn while it slides away.
    pub last_panel: Option<Panel>,
    /// The destination (Settings, Networks) under a deep panel.
    pub base: Option<Panel>,
    /// Reduced motion: where the crossfade is heading and since when.
    fade: (bool, Option<Instant>),
    /// Android's back gesture under way over the open chat: where it holds the chat (0 to 1).
    gesture: Option<f32>,
    /// The gesture let go: the chat travels from where it was left, to 0 or back to 1.
    settle: Option<(f32, f32, Instant, Duration)>,
}

pub(super) struct Layers {
    pub under_x: f32,
    pub veil: f32,
    pub over_x: f32,
    pub over_alpha: f32,
}

/// Where the two layers sit at `v` (0 list alone, 1 chat alone) in a window `w` wide. Reduced motion
/// does not slide: the chat fades in over the list.
pub(super) fn layers(v: f32, w: f32, reduced: bool) -> Layers {
    if reduced {
        Layers { under_x: 0., veil: 0., over_x: 0., over_alpha: v }
    } else {
        Layers { under_x: -w * motion::PUSH_PARALLAX * v, veil: motion::PUSH_VEIL * v, over_x: w * (1. - v), over_alpha: 1. }
    }
}

impl Slide {
    /// 0..1, retargeted from wherever it is, so a push reversed half way turns around where it stands.
    pub(super) fn value(&mut self, wanted: bool, window: &mut Window, cx: &mut App) -> f32 {
        let jump = std::mem::take(&mut self.jump);
        if cx.reduce_motion() {
            if self.fade.0 != wanted {
                self.fade = (wanted, (!jump).then(Instant::now));
            }
            let t = self.fade.1.map_or(1., |at| at.elapsed().as_secs_f32() / motion::PUSH_FADE.as_secs_f32());
            if t < 1. {
                window.request_animation_frame();
            }
            let t = motion::ease_out(t.min(1.));
            return if wanted { t } else { 1. - t };
        }
        let policy = if jump {
            Transition::new(Duration::ZERO)
        } else if wanted {
            Transition::new(motion::PUSH_IN).easing(motion::EASE_SHEET)
        } else {
            Transition::new(motion::PUSH).easing(motion::EASE_OUT)
        };
        let base = transition("nav", if wanted { 1.0f32 } else { 0. }, policy, window, cx);
        if let Some(v) = self.gesture {
            return v;
        }
        // A gesture let go goes on from where the finger left the chat, until the push has caught up.
        if let Some((from, to, at, dur)) = self.settle {
            let t = at.elapsed().as_secs_f32() / dur.as_secs_f32();
            if t >= 1. && base == to {
                self.settle = None;
            } else {
                window.request_animation_frame();
                return if t >= 1. { to } else { from + (to - from) * motion::ease_out(t) };
            }
        }
        base
    }
}

/// Where the chat is held for a back gesture whose finger, from the left edge, is at `x` of a
/// window `w` wide: its left edge under the finger, as on iOS.
#[cfg(any(target_os = "android", test))]
fn held_at(x: f32, w: f32) -> f32 {
    if w <= 0. { 1. } else { (1. - x / w).clamp(0., 1.) }
}

impl BeeperApp {
    /// Whether back now pops the open chat on a phone, with nothing over it for back to close
    /// first. Kept in step with `go_back`, whose earlier branches each rule it out here.
    #[cfg(target_os = "android")]
    fn back_pops_chat(&self, window: &Window) -> bool {
        let narrow = frame::viewport(window).width < px(NARROW);
        narrow
            && self.open.is_some()
            && self.slide.leaving.is_none()
            && self.verify.sheet.is_none()
            && !self.share.open
            && self.viewer.is_none()
            && self.people.confirm.is_none()
            && self.fwd.deleting.is_none()
            && !(self.preview.open() && !caption::tray())
            && !self.menu_open()
            && self.audience.open.is_none()
            && self.chat_sheet.is_none()
            && !self.gallery.menu
            && !self.keys.open
            && !self.location.open
            && !self.poll_form.open
            && self.fwd.picking.is_none()
            && !self.gallery.open
            && !self.gifs_open()
            && !self.emoji_pick.tones
            && self.emoji_open.is_none()
            && !self.mention.open()
            && !self.emoji_hint.open()
            && self.fwd.selected.is_none()
            && !self.info_open()
            && self.compose == Compose::New
            && self.panel.is_none()
            && !self.find.open
            && !self.searching
    }

    /// Android's back gesture moved or ended: over the open chat the chat follows the finger and
    /// goes or comes back with it; anywhere else its end is back, as Escape.
    #[cfg(target_os = "android")]
    pub(super) fn on_back_gesture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if let Some(back) = crate::back_gesture::take_end() {
            let held = self.slide.gesture.take();
            match (back, held) {
                (true, Some(v)) => {
                    self.slide.settle = Some((v, 0., now, motion::PUSH.mul_f32(v.max(0.3))));
                    self.pop_room(cx);
                }
                (true, None) => {
                    self.go_back(window, cx);
                }
                (false, Some(v)) => self.slide.settle = Some((v, 1., now, motion::PUSH.mul_f32((1. - v).max(0.3)))),
                (false, None) => {}
            }
            cx.notify();
            return;
        }
        let Some(left_x) = crate::back_gesture::tracking() else { return };
        // Only the left edge has a place on iOS; from the right, back waits for the gesture's end.
        let Some(x) = left_x.filter(|_| !cx.reduce_motion() && (self.slide.gesture.is_some() || self.back_pops_chat(window))) else { return };
        let w = f32::from(frame::viewport(window).width);
        self.slide.settle = None;
        self.slide.gesture = Some(held_at(x / window.scale_factor(), w));
        cx.notify();
    }

    /// Pops the open chat: it slides off and closes where it lands. Reduced motion closes at once.
    pub(super) fn pop_room(&mut self, cx: &mut Context<Self>) {
        if cx.reduce_motion() || self.slide.leaving.is_some() {
            if self.slide.leaving.is_none() {
                self.close_room(cx);
            }
            cx.notify();
            return;
        }
        let at = Instant::now();
        self.slide.leaving = Some(at);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(motion::PUSH).await;
            let _ = this.update(cx, |this, cx| {
                if this.slide.leaving == Some(at) {
                    this.close_room(cx);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

/// What a new message did to the order of the rows on screen.
#[derive(Debug, PartialEq)]
pub(super) struct Reorder<T> {
    /// Rows pushed down, with how many rows above their new place they started (always 1: a row jumped over them).
    pub shifted: Vec<(T, i32)>,
    /// The row that came to the top: it only fades in, the list paints in order.
    pub entered: Option<T>,
}

/// None when the change is no single row jumping up (a sort, a filter, a sync).
pub(super) fn reorder<T: PartialEq + Clone>(before: &[T], after: &[T]) -> Option<Reorder<T>> {
    let mut out = Reorder { shifted: Vec::new(), entered: None };
    for (new, id) in after.iter().take(TRACKED).enumerate() {
        match before.iter().position(|b| b == id) {
            Some(old) if old == new => {}
            Some(old) if old > new => {
                if out.entered.replace(id.clone()).is_some() {
                    return None;
                }
            }
            Some(old) if new == old + 1 => out.shifted.push((id.clone(), 1)),
            Some(_) => return None,
            None if new == 0 && out.entered.is_none() => out.entered = Some(id.clone()),
            None => return None,
        }
    }
    (out.shifted.len() + usize::from(out.entered.is_some()) > 0).then_some(out)
}

#[derive(Default)]
pub(super) struct Fly {
    glides: HashMap<OwnedRoomId, Glide>,
    fade: Option<(OwnedRoomId, Instant)>,
}

/// The rows on screen, remembered before a batch of changes.
pub(super) fn snapshot(shown: &[usize], rooms: &[RoomSummary]) -> Vec<OwnedRoomId> {
    shown.iter().take(TRACKED).map(|&ix| rooms[ix].id.clone()).collect()
}

impl Fly {
    pub(super) fn quiet(changed: usize) -> bool {
        changed > MAX_CHANGED
    }

    pub(super) fn start(&mut self, change: Reorder<OwnedRoomId>, row_h: f32) {
        self.glides.clear();
        self.fade = None;
        let now = Instant::now();
        for (id, rows) in change.shifted {
            let mut g = Glide::new(motion::ROW_SETTLE.0, motion::ROW_SETTLE.1, 0.5, -(rows as f32) * row_h);
            g.release(0., 0., now);
            self.glides.insert(id, g);
        }
        self.fade = change.entered.map(|id| (id, now));
    }

    /// Vertical offset of a row, and its opacity; asks for frames while either moves.
    pub(super) fn of(&mut self, id: &OwnedRoomId, window: &mut Window) -> (f32, f32) {
        let mut y = 0.;
        if let Some(g) = self.glides.get_mut(id) {
            y = g.step(Instant::now());
            if g.running() {
                window.request_animation_frame();
            } else {
                self.glides.remove(id);
            }
        }
        let mut alpha = 1.;
        if let Some((fid, at)) = &self.fade
            && fid == id
        {
            let t = at.elapsed().as_secs_f32() / motion::ENTER.as_secs_f32();
            if t < 1. {
                alpha = motion::ease_out(t);
                window.request_animation_frame();
            } else {
                self.fade = None;
            }
        }
        (y, alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::{held_at, layers, reorder};

    #[test]
    fn a_back_gesture_holds_the_chat_under_the_finger() {
        assert_eq!(held_at(0., 400.), 1.);
        assert_eq!(held_at(100., 400.), 0.75);
        assert_eq!(held_at(900., 400.), 0.);
    }

    #[test]
    fn a_row_jumping_up_pushes_the_ones_above_it_down() {
        let r = reorder(&["a", "b", "c", "d"], &["c", "a", "b", "d"]).unwrap();
        assert_eq!(r.entered, Some("c"));
        assert_eq!(r.shifted, vec![("a", 1), ("b", 1)]);
    }

    #[test]
    fn a_new_row_at_the_top_fades_in_and_nothing_else_is_left() {
        let r = reorder(&["a", "b"], &["n", "a", "b"]).unwrap();
        assert_eq!((r.entered, r.shifted.len()), (Some("n"), 2));
    }

    #[test]
    fn no_change_and_a_reshuffle_make_no_motion() {
        assert_eq!(reorder(&["a", "b"], &["a", "b"]), None);
        assert_eq!(reorder(&["a", "b", "c"], &["c", "b", "a"]), None);
    }

    #[test]
    fn layers_slide_or_fade() {
        let l = layers(0.5, 400., false);
        assert_eq!((l.under_x, l.over_x, l.veil), (-50., 200., 0.25));
        let r = layers(0.5, 400., true);
        assert_eq!((r.under_x, r.over_x, r.over_alpha), (0., 0., 0.5));
    }
}
