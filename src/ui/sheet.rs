//! A bottom sheet's motion, one for every sheet: it rises on the sheet curve, falls quicker, follows a
//! finger on its header 1:1 and lands with the finger's speed. Its height comes from a probe, so no
//! sheet guesses it. Driven by whether the sheet should be up, sampled during render, so the code
//! that opens and closes a sheet only sets its own state.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::gallery::speed;
use super::motion::Glide;
use super::*;

/// A downward flick this fast (px/s) dismisses the sheet however short it was.
const DISMISS_SPEED: f32 = 800.;
/// Past this share of its height a sheet let go goes, unless flung back up faster than `KEEP_SPEED`.
const DISMISS_SHARE: f32 = 0.3;
const KEEP_SPEED: f32 = -300.;
/// A thrown sheet aims this far past its height, so it leaves at speed instead of creeping to the edge.
const THROW_PAST: f32 = 40.;
/// Drawn this far down until the probe has measured it: one frame, out of sight.
const UNMEASURED: f32 = 4000.;
const MAX_SAMPLES: usize = 6;
const SCRIM: f32 = 0.6;

/// Whether a sheet let go at `y` px below its open position, moving at `speed` px/s, closes.
pub(super) fn dismisses(y: f32, speed: f32, h: f32) -> bool {
    (y > 0. && speed > DISMISS_SPEED) || (y > h * DISMISS_SHARE && speed > KEEP_SPEED)
}

/// Up past the open position the sheet follows a fifth of the way, so it meets resistance.
pub(super) fn follow(raw: f32) -> f32 {
    if raw >= 0. { raw } else { raw * 0.2 }
}

fn unfollow(y: f32) -> f32 {
    if y >= 0. { y } else { y / 0.2 }
}

#[derive(Default)]
enum Drive {
    /// At rest: open while live, gone while not.
    #[default]
    Still,
    /// Eased between shares of the height (0 open, 1 hidden); its clock starts once the sheet is measured.
    Tween { from: f32, to: f32, delay: Duration, dur: Duration, ease: fn(f32) -> f32, start: Option<Instant> },
    /// Released by a finger, in px.
    Spring(Glide),
    /// Held: `from` where it was caught (before resistance), `moved` the finger's travel since.
    Finger { from: f32, moved: f32, samples: VecDeque<(Instant, f32)> },
}

/// What one frame of a sheet draws.
#[derive(Clone, Copy, Debug)]
pub(super) struct SheetFrame {
    /// How far below its open position the sheet is drawn, px.
    pub y: f32,
    pub scrim: f32,
    /// Takes input; false while it falls.
    pub live: bool,
}

#[derive(Default)]
pub(super) struct SheetMotion {
    live: bool,
    mounted: bool,
    drive: Drive,
    y: f32,
    h: Rc<Cell<f32>>,
    /// Natural height of the content `fit` wraps.
    fit: Rc<Cell<f32>>,
    /// Which content `fit` drew last, and when it changed to it.
    fit_key: Cell<Option<(usize, Option<Instant>)>>,
    /// Opened over a sheet that is leaving: the scrim is already there, so it is full at once, and
    /// the rise waits `HANDOFF_DELAY`.
    pub handoff: bool,
    /// Leaving under the sheet that replaces it: no scrim, and quicker.
    pub quiet: bool,
    /// When the rise began, for what staggers in with it.
    risen: Option<Instant>,
}

impl SheetMotion {
    /// Steps the sheet to this frame. `None` once it is closed and out of sight: the caller then
    /// forgets what it kept to draw the fall.
    pub(super) fn sample(&mut self, live: bool, window: &mut Window, cx: &App) -> Option<SheetFrame> {
        let now = Instant::now();
        self.set_live(live);
        if !self.mounted {
            return None;
        }
        let y = self.advance(now, cx.reduce_motion());
        if !self.mounted {
            return None;
        }
        if self.moving() {
            window.request_animation_frame();
        }
        Some(SheetFrame { y, scrim: self.scrim(y), live: self.live })
    }

    fn set_live(&mut self, live: bool) {
        if live == self.live {
            return;
        }
        self.live = live;
        let from = self.share();
        if live {
            self.mounted = true;
            let delay = if self.handoff { motion::HANDOFF_DELAY } else { Duration::ZERO };
            self.drive = Drive::Tween { from, to: 0., delay, dur: motion::SHEET_IN, ease: motion::ease_sheet, start: None };
            self.risen = None;
        } else if self.mounted {
            self.handoff = false;
            let dur = if self.quiet { motion::SHEET_HANDOFF } else { motion::SHEET_OUT };
            self.drive = Drive::Tween { from, to: 1., delay: Duration::ZERO, dur, ease: motion::ease_out, start: None };
        }
    }

    /// The drawn offset as a share of the height; 1 when hidden or not measured.
    fn share(&self) -> f32 {
        let h = self.h.get();
        if !self.mounted || h <= 0. { 1. } else { (self.y / h).min(1.) }
    }

    fn unmount(&mut self) {
        (self.mounted, self.drive, self.quiet, self.handoff, self.risen, self.y) = (false, Drive::Still, false, false, None, 0.);
        self.fit.set(0.);
        self.fit_key.set(None);
    }

    fn advance(&mut self, now: Instant, still: bool) -> f32 {
        let h = self.h.get();
        if h <= 0. {
            if !self.live {
                self.unmount();
            }
            return UNMEASURED;
        }
        let live = self.live;
        let y = match &mut self.drive {
            Drive::Still if live => 0.,
            Drive::Still => {
                self.unmount();
                return h;
            }
            Drive::Tween { from, to, delay, dur, ease, start } => {
                let start = *start.get_or_insert(now);
                let (from, to, delay, dur, ease) = (*from, *to, *delay, *dur, *ease);
                let t = if still { 1. } else { (now.saturating_duration_since(start).saturating_sub(delay).as_secs_f32() / dur.as_secs_f32()).min(1.) };
                if to == 0. && !still && now >= start + delay {
                    self.risen.get_or_insert(start + delay);
                }
                if t >= 1. {
                    self.drive = Drive::Still;
                    self.handoff = false;
                    if to >= 1. {
                        self.unmount();
                        return h;
                    }
                }
                (from + (to - from) * ease(t)) * h
            }
            Drive::Spring(glide) => {
                if still {
                    glide.set(glide.target());
                }
                let y = glide.step(now);
                if !live && y >= h {
                    self.unmount();
                    return h;
                }
                if !glide.running() {
                    self.drive = Drive::Still;
                }
                y
            }
            Drive::Finger { from, moved, .. } => follow(*from + *moved),
        };
        self.y = y;
        y
    }

    fn moving(&self) -> bool {
        match &self.drive {
            Drive::Tween { .. } => true,
            Drive::Spring(glide) => glide.running(),
            Drive::Still | Drive::Finger { .. } => false,
        }
    }

    fn scrim(&self, y: f32) -> f32 {
        let h = self.h.get();
        if !self.live && self.quiet {
            0.
        } else if self.live && self.handoff {
            SCRIM
        } else if h <= 0. {
            0.
        } else {
            SCRIM * (1. - y / h).clamp(0., 1.)
        }
    }

    /// How long ago the rise began, while it is up.
    pub(super) fn risen_for(&self) -> Option<Duration> {
        self.risen.filter(|_| self.live).map(|at| at.elapsed())
    }

    /// Follows a finger on the sheet's header. True when it let go so as to close: the sheet is then
    /// already falling, and the caller only closes its own state.
    pub(super) fn drag(&mut self, e: &ScrollWheelEvent) -> bool {
        let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
        self.touch(e.touch_phase, dy, Instant::now())
    }

    fn touch(&mut self, phase: TouchPhase, dy: f32, now: Instant) -> bool {
        // A falling sheet is committed; only one up or rising can be caught.
        if !self.live || !self.mounted {
            return false;
        }
        match phase {
            TouchPhase::Started => {
                self.handoff = false;
                self.drive = Drive::Finger { from: unfollow(self.y), moved: dy, samples: VecDeque::from([(now, dy)]) };
            }
            // A mouse wheel has no start, so it never drags.
            TouchPhase::Moved => {
                if let Drive::Finger { moved, samples, .. } = &mut self.drive {
                    *moved += dy;
                    samples.push_back((now, *moved));
                    if samples.len() > MAX_SAMPLES {
                        samples.pop_front();
                    }
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Drive::Finger { from, moved, mut samples } = std::mem::take(&mut self.drive) else { return false };
                // A finger that rested before lifting has no speed left.
                samples.push_back((now, moved));
                let speed = if phase == TouchPhase::Ended { speed(&samples) } else { 0. };
                let y = follow(from + moved);
                let h = self.h.get();
                self.y = y;
                if phase == TouchPhase::Ended && dismisses(y, speed, h) {
                    let mut glide = Glide::new(motion::THROW.0, motion::THROW.1, 0.5, y);
                    glide.release(h + THROW_PAST, speed, now);
                    (self.live, self.drive) = (false, Drive::Spring(glide));
                    return true;
                }
                let mut glide = Glide::new(motion::SETTLE.0, motion::SETTLE.1, 0.5, y);
                glide.release(0., speed, now);
                self.drive = Drive::Spring(glide);
            }
        }
        false
    }

    /// Put inside the sheet's card: measures it after layout and asks for a frame when it changed.
    pub(super) fn probe(&self, view: EntityId) -> impl IntoElement + use<> {
        probe(self.h.clone(), view)
    }

    /// Content whose height can change while the sheet is up: the box follows it in `ENTER`, clipped,
    /// and content under a new `key` fades in, instead of the sheet jumping. What it opened with is
    /// simply there.
    pub(super) fn fit(&self, id: &'static str, key: usize, content: impl IntoElement, view: EntityId, window: &mut Window, cx: &mut App) -> Div {
        let natural = self.fit.get();
        let tall = (natural > 0.).then(|| transition(id, natural, motion::enter(), window, cx));
        let since = match self.fit_key.get() {
            Some((old, since)) if old == key => since,
            Some(_) if !cx.reduce_motion() => Some(Instant::now()),
            _ => None,
        };
        self.fit_key.set(Some((key, since)));
        let fade = since.map_or(1., |at| motion::ease_out((at.elapsed().as_secs_f32() / motion::FADE_IN.as_secs_f32()).min(1.)));
        if fade < 1. {
            window.request_animation_frame();
        }
        v_flex()
            .flex_none()
            .overflow_hidden()
            .when_some(tall, |el, tall| el.h(px(tall)))
            .child(div().relative().flex_none().opacity(fade).child(content).child(probe(self.fit.clone(), view)))
    }
}

fn probe(cell: Rc<Cell<f32>>, view: EntityId) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let got = f32::from(bounds.size.height);
            if (got - cell.get()).abs() > 0.5 {
                cell.set(got);
                // During prepaint, so not `request_animation_frame`, which needs a view being rendered.
                window.on_next_frame(move |_, cx| cx.notify(view));
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// The pill on a sheet's top that says it can be dragged down.
pub(super) fn handle(ink: Hsla) -> Div {
    div().flex_none().h(px(20.)).flex().items_center().justify_center().child(div().w(px(36.)).h(px(4.)).rounded_full().bg(ink.opacity(0.5)))
}

/// The dimmed layer a sheet rises in; it blocks the screen under it only while the sheet is live.
pub(super) fn scrim(id: &'static str, frame: SheetFrame) -> Stateful<Div> {
    div().id(id).absolute().inset_0().flex().flex_col().justify_end().items_center().bg(black().opacity(frame.scrim)).when(frame.live, |el| el.occlude())
}

/// Entry `i` of what rises with a sheet, 0 to 1: `ENTER` long, `first` after the rise began and
/// `step` after the one before. 1 when there is no rise to follow.
pub(super) fn stagger(risen: Option<Duration>, i: usize, first: Duration, step: Duration) -> f32 {
    let Some(risen) = risen else { return 1. };
    let t = risen.saturating_sub(first + step * i as u32).as_secs_f32() / motion::ENTER.as_secs_f32();
    motion::ease_out(t.min(1.))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(8);

    fn up(h: f32) -> (SheetMotion, Instant) {
        let mut s = SheetMotion::default();
        s.h.set(h);
        let t0 = Instant::now();
        s.set_live(true);
        s.advance(t0, false);
        s.advance(t0 + Duration::from_secs(1), false);
        (s, t0 + Duration::from_secs(1))
    }

    #[test]
    fn a_long_drag_or_a_quick_flick_dismisses() {
        assert!(dismisses(40., 1000., 600.));
        assert!(!dismisses(60., 50., 600.), "short and slow springs back");
        assert!(dismisses(200., 0., 600.), "past 30 %");
        assert!(!dismisses(200., -500., 600.), "past 30 % but flung back up");
        assert!(!dismisses(-20., 2000., 600.), "above the open position");
    }

    #[test]
    fn opens_from_below_and_rests_open() {
        let mut s = SheetMotion::default();
        s.h.set(500.);
        let t0 = Instant::now();
        s.set_live(true);
        assert_eq!(s.advance(t0, false), 500.);
        let mid = s.advance(t0 + Duration::from_millis(100), false);
        assert!(mid > 0. && mid < 500.);
        assert_eq!(s.advance(t0 + motion::SHEET_IN, false), 0.);
        assert!(!s.moving());
        assert_eq!(s.scrim(0.), SCRIM);
    }

    #[test]
    fn a_flick_throws_it_out_and_unmounts() {
        let (mut s, t) = up(500.);
        s.touch(TouchPhase::Started, 0., t);
        for i in 1..=4 {
            s.touch(TouchPhase::Moved, 15., t + FRAME * i);
        }
        assert_eq!(s.advance(t + FRAME * 4, false), 60.);
        assert!(s.touch(TouchPhase::Ended, 0., t + FRAME * 5));
        assert!(!s.live);
        let mut last = 60.;
        for i in 6..200 {
            let at = t + FRAME * i;
            if !s.mounted {
                break;
            }
            let y = s.advance(at, false);
            assert!(y >= last, "falls without turning back");
            last = y;
        }
        assert!(!s.mounted, "gone once below its height");
    }

    #[test]
    fn a_short_drag_settles_back_without_passing_the_edge() {
        let (mut s, t) = up(500.);
        s.touch(TouchPhase::Started, 0., t);
        s.touch(TouchPhase::Moved, 40., t + Duration::from_millis(500));
        assert!(!s.touch(TouchPhase::Ended, 0., t + Duration::from_millis(600)));
        for i in 1..100 {
            assert!(s.advance(t + Duration::from_millis(600) + FRAME * i, false) >= 0.);
        }
        assert!(!s.moving());
        assert_eq!(s.y, 0.);
    }

    #[test]
    fn upward_drag_meets_resistance_and_can_be_caught_again() {
        let (mut s, t) = up(500.);
        s.touch(TouchPhase::Started, 0., t);
        s.touch(TouchPhase::Moved, -100., t + FRAME);
        assert_eq!(s.advance(t + FRAME, false), -20.);
        s.touch(TouchPhase::Ended, 0., t + FRAME * 30);
        let y = s.advance(t + FRAME * 31, false);
        s.touch(TouchPhase::Started, 0., t + FRAME * 31);
        assert!((s.advance(t + FRAME * 31, false) - y).abs() < 0.01, "caught where it was");
    }

    #[test]
    fn closing_by_button_falls_from_where_it_is() {
        let (mut s, t) = up(400.);
        s.set_live(false);
        assert_eq!(s.advance(t, false), 0.);
        assert!(s.advance(t + Duration::from_millis(100), false) > 0.);
        s.advance(t + motion::SHEET_OUT, false);
        assert!(!s.mounted);
    }

    #[test]
    fn a_falling_sheet_cannot_be_caught() {
        let (mut s, t) = up(400.);
        s.set_live(false);
        s.advance(t + FRAME, false);
        s.touch(TouchPhase::Started, 0., t + FRAME);
        assert!(!s.live && s.mounted);
        let mut d = Duration::ZERO;
        while s.mounted && d < Duration::from_secs(1) {
            d += FRAME;
            s.advance(t + d, false);
        }
        assert!(!s.mounted);
    }

    #[test]
    fn handoff_keeps_the_scrim_full_and_quiet_draws_none() {
        let mut s = SheetMotion { handoff: true, ..Default::default() };
        s.h.set(300.);
        let t0 = Instant::now();
        s.set_live(true);
        let y = s.advance(t0 + Duration::from_millis(30), false);
        assert_eq!((y, s.scrim(y)), (300., SCRIM), "waits, scrim already full");
        let (mut out, t) = up(300.);
        out.quiet = true;
        out.set_live(false);
        let y = out.advance(t + Duration::from_millis(50), false);
        assert_eq!(out.scrim(y), 0.);
        out.advance(t + motion::SHEET_HANDOFF, false);
        assert!(!out.mounted && !out.quiet);
    }

    #[test]
    fn reduce_motion_jumps() {
        let mut s = SheetMotion::default();
        s.h.set(300.);
        let t0 = Instant::now();
        s.set_live(true);
        assert_eq!(s.advance(t0, true), 0.);
        s.set_live(false);
        s.advance(t0, true);
        assert!(!s.mounted);
    }

    #[test]
    fn staggered_entries_start_in_turn() {
        let at = |ms| Some(Duration::from_millis(ms));
        let (first, step) = (Duration::from_millis(160), Duration::from_millis(25));
        assert_eq!(stagger(at(150), 0, first, step), 0.);
        assert!(stagger(at(200), 0, first, step) > 0.);
        assert_eq!(stagger(at(200), 2, first, step), 0.);
        assert_eq!(stagger(at(1000), 4, first, step), 1.);
        assert_eq!(stagger(None, 3, first, step), 1.);
    }
}
