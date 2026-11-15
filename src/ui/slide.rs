//! A phone's screens stacked: the list underneath, a chat or a deep panel pushed over it, and the
//! chat list's rows settling when a new message reorders them.

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
        transition("nav", if wanted { 1.0f32 } else { 0. }, policy, window, cx)
    }
}

impl BeeperApp {
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
    use super::*;

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
