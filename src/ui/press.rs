//! Press feedback that works with a finger. A touch reaches GPUI as a click only when it lifts, so
//! `.active()` never shows on a phone; this reads the touch itself, lights a tint at once and fades it
//! out on release. Also the pill that slides between the items of a segmented control.

use std::cell::RefCell;
use std::rc::Rc;

use super::motion;
use super::*;

/// A finger that moves further than this is scrolling, not pressing.
const SLOP: f32 = 8.;

#[derive(Default)]
struct Press {
    down: Option<(Instant, Point<Pixels>)>,
}

fn child_id(id: &ElementId, name: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), name.into())
}

fn set(state: &Entity<Press>, cx: &mut App, down: Option<(Instant, Point<Pixels>)>) {
    state.update(cx, |press, cx| {
        if press.down.is_some() != down.is_some() {
            press.down = down;
            cx.notify();
        }
    });
}

/// The tint over a pressable thing, for the thing's `.relative()` parent. It shows `delay` after the
/// finger lands (zero outside scrolling containers, so a scroll never lights it) and fades in
/// `EXIT` after release.
/// Round the corners of the result to match the parent.
pub(super) fn area(id: impl Into<ElementId>, delay: Duration, tint: Hsla, window: &mut Window, cx: &mut App) -> Div {
    let id: ElementId = id.into();
    let state = window.use_keyed_state(child_id(&id, "press"), cx, |_, _| Press::default());
    let down = state.read(cx).down;
    let waiting = down.is_some_and(|(at, _)| at.elapsed() < delay);
    if waiting {
        window.request_animation_frame();
    }
    let lit = down.is_some() && !waiting;
    let policy = if lit { Transition::new(Duration::ZERO) } else { motion::exit() };
    let alpha = transition(child_id(&id, "fade"), if lit { 1.0f32 } else { 0. }, policy, window, cx);
    let probe = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let visible = window.content_mask().bounds;
            let s = state.clone();
            window.on_mouse_event(move |e: &TouchDragEvent, phase, _, cx| {
                if !phase.bubble() {
                    return;
                }
                match e.phase {
                    TouchPhase::Started if bounds.contains(&e.start_position) && visible.contains(&e.start_position) => {
                        set(&s, cx, Some((Instant::now(), e.start_position)));
                    }
                    TouchPhase::Moved => {
                        let far = s.read(cx).down.is_some_and(|(_, at)| f32::from(e.position.x - at.x).hypot(f32::from(e.position.y - at.y)) > SLOP);
                        if far {
                            set(&s, cx, None);
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => set(&s, cx, None),
                    TouchPhase::Started => {}
                }
            });
            let s = state.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase.bubble() {
                    set(&s, cx, None);
                }
            });
            let s = state.clone();
            window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| {
                if phase.bubble() {
                    set(&s, cx, None);
                }
            });
            let s = state.clone();
            window.on_mouse_event(move |e: &LongPressEvent, phase, _, cx| {
                if phase.bubble() && matches!(e.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                    set(&s, cx, None);
                }
            });
        },
    )
    .absolute()
    .inset_0();
    div().absolute().inset_0().bg(tint).opacity(alpha).child(probe)
}

/// A touch's long press on the `.relative()` parent. GPUI never turns one into a secondary click, and
/// an unclaimed one still ends as a tap, so this claims it: the lift then does not click the parent.
pub(super) fn long_press(on: impl Fn(&LongPressEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            window.on_mouse_event(move |e: &LongPressEvent, phase, window, cx| {
                if phase.bubble() && e.phase == TouchPhase::Started && !window.default_prevented() && hitbox.is_hovered(window) {
                    window.prevent_default();
                    cx.stop_propagation();
                    on(e, window, cx);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

const ITEMS: usize = 8;

/// Bounds of a segmented control's items, measured by last frame's paint.
#[derive(Clone)]
pub(super) struct Segments(Rc<RefCell<[Option<Bounds<Pixels>>; ITEMS + 1]>>);

impl Segments {
    pub(super) fn new(id: &'static str, window: &mut Window, cx: &mut App) -> Self {
        window.use_keyed_state(id, cx, |_, _| Segments(Rc::default())).read(cx).clone()
    }

    /// Measures item `ix` (or, as `ITEMS`, the container); goes inside the item as a child.
    pub(super) fn probe(&self, ix: usize) -> impl IntoElement + use<> {
        let cells = self.0.clone();
        canvas(move |bounds, _, _| cells.borrow_mut()[ix.min(ITEMS)] = Some(bounds), |_, _, _, _| {}).absolute().inset_0()
    }

    pub(super) fn container(&self) -> impl IntoElement + use<> {
        self.probe(ITEMS)
    }

    /// The pill behind item `selected`, sliding from the old item; None until the items are measured
    /// (the caller paints the selected item itself for that one frame).
    pub(super) fn pill(&self, id: &'static str, selected: usize, tint: Hsla, window: &mut Window, cx: &mut App) -> Option<Div> {
        let cells = self.0.borrow();
        let (Some(to), Some(root)) = (cells[selected.min(ITEMS)], cells[ITEMS]) else {
            window.request_animation_frame();
            return None;
        };
        // Relative to the control: animating window coordinates made the pill chase every scroll.
        let at = Bounds { origin: point(to.origin.x - root.origin.x, to.origin.y - root.origin.y), size: to.size };
        let b = transition(id, at, motion::enter(), window, cx);
        Some(
            div()
                .absolute()
                .left(b.origin.x)
                .top(b.origin.y)
                .w(b.size.width)
                .h(b.size.height)
                .rounded_full()
                .bg(tint),
        )
    }
}
