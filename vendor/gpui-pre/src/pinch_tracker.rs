// murmur patch: new file, see vendor/gpui-pre/PATCHES.md
//! Two fingers on the screen as a pinch. The touch recognizer ignores a second contact, so the
//! window feeds every raw touch here as well and gets [`PinchEvent`]s back, the same events a
//! trackpad pinch produces.

use crate::{Modifiers, PinchEvent, Pixels, Point, TouchEvent, TouchId, TouchPhase};

/// At most two contacts are followed; a third finger is ignored.
#[derive(Default)]
pub(crate) struct PinchTracker {
    touches: Vec<(TouchId, Point<Pixels>)>,
    /// The distance between the fingers at the last event, while a pinch is going.
    span: Option<f32>,
}

fn distance(a: Point<Pixels>, b: Point<Pixels>) -> f32 {
    let (dx, dy) = (f32::from(a.x - b.x), f32::from(a.y - b.y));
    dx.hypot(dy)
}

impl PinchTracker {
    fn midpoint(&self) -> Point<Pixels> {
        match self.touches[..] {
            [(_, a), (_, b)] => Point { x: (a.x + b.x) / 2., y: (a.y + b.y) / 2. },
            _ => self.touches.first().map(|t| t.1).unwrap_or_default(),
        }
    }

    fn event(&self, delta: f32, phase: TouchPhase) -> PinchEvent {
        PinchEvent { position: self.midpoint(), delta, modifiers: Modifiers::default(), phase }
    }

    pub(crate) fn handle_event(&mut self, touch: &TouchEvent) -> Option<PinchEvent> {
        match touch.phase {
            TouchPhase::Started => {
                if self.touches.len() < 2 && !self.touches.iter().any(|t| t.0 == touch.id) {
                    self.touches.push((touch.id, touch.position));
                }
                if self.touches.len() == 2 && self.span.is_none() {
                    self.span = Some(distance(self.touches[0].1, self.touches[1].1).max(1.));
                    return Some(self.event(0., TouchPhase::Started));
                }
                None
            }
            TouchPhase::Moved => {
                self.touches.iter_mut().find(|t| t.0 == touch.id)?.1 = touch.position;
                let last = self.span?;
                let now = distance(self.touches[0].1, self.touches[1].1).max(1.);
                self.span = Some(now);
                Some(self.event(now / last - 1., TouchPhase::Moved))
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let ended = self.span.is_some().then(|| self.event(0., touch.phase));
                self.touches.retain(|t| t.0 != touch.id);
                if ended.is_some() {
                    self.span = None;
                }
                ended
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::px;

    fn touch(id: u64, x: f32, y: f32, phase: TouchPhase) -> TouchEvent {
        TouchEvent { id: TouchId(id), phase, position: Point { x: px(x), y: px(y) }, ..Default::default() }
    }

    #[test]
    fn two_fingers_pinch_and_one_does_not() {
        let mut t = PinchTracker::default();
        assert!(t.handle_event(&touch(1, 0., 0., TouchPhase::Started)).is_none());
        assert!(t.handle_event(&touch(1, 5., 0., TouchPhase::Moved)).is_none());
        let start = t.handle_event(&touch(2, 100., 0., TouchPhase::Started)).unwrap();
        assert_eq!(start.phase, TouchPhase::Started);
        let spread = t.handle_event(&touch(2, 195., 0., TouchPhase::Moved)).unwrap();
        assert!((spread.delta - 1.).abs() < 1e-4, "95 px to 190 px doubles");
        assert!((f32::from(spread.position.x) - 100.).abs() < 1e-3);
        assert!(t.handle_event(&touch(3, 9., 9., TouchPhase::Started)).is_none(), "a third finger");
        assert_eq!(t.handle_event(&touch(2, 195., 0., TouchPhase::Ended)).unwrap().phase, TouchPhase::Ended);
        assert!(t.handle_event(&touch(1, 9., 0., TouchPhase::Moved)).is_none());
    }
}
