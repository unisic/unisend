//! A chat row swiped sideways acts on the chat, Gmail style: the row follows the finger once the pan
//! is sideways, the action's colour and icon show behind it, and letting go past the arm point (or
//! flicking) runs it. Archive and delete carry the row off and the rows below close the gap.

use std::collections::VecDeque;

use super::motion::{self, Glide};
use super::*;

/// Past this share of the row's width a release acts.
const ARM_SHARE: f32 = 0.3;
const ARM_MIN: f32 = 72.;
const ARM_MAX: f32 = 140.;
/// A flick acts short of the arm point; faster than the reply flick, as these act on a whole chat.
const FLICK_SPEED: f32 = 1000.;
const FLICK_MIN: f32 = 32.;
/// Rows below a removed one that slide up into its place: more than a phone shows.
const GAP_ROWS: usize = 24;
/// The fill behind the row brightens to the action's colour this fast when it arms.
const LIGHT: Duration = Duration::from_millis(150);
/// The fill's opacity before it arms.
const FAINT: f32 = 0.2;
/// A row carried off comes back if its chat has not left the list by then: a sync that stalls.
const AWAY_FOR: Duration = Duration::from_secs(10);

/// What a swipe one way does, chosen in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum SwipeAction {
    None,
    Archive,
    Pin,
    LowPriority,
    Mute,
    Unread,
    Delete,
}

impl SwipeAction {
    pub(crate) const ALL: [Self; 7] = [Self::Archive, Self::Unread, Self::Pin, Self::Mute, Self::LowPriority, Self::Delete, Self::None];

    /// Right to left.
    pub(crate) fn left_default() -> Self {
        Self::Archive
    }

    /// Left to right.
    pub(crate) fn right_default() -> Self {
        Self::Unread
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::None => t!("Off"),
            Self::Archive => t!("Archive|verb"),
            Self::Pin => t!("Pin"),
            Self::LowPriority => t!("Low priority"),
            Self::Mute => t!("Mute"),
            Self::Unread => t!("Read or unread"),
            Self::Delete => t!("Delete"),
        }
    }
}

/// What a swipe does to this chat now: a chat action as the chat sheet words it, or deleting.
#[derive(Clone, Copy, Debug)]
pub(super) enum Act {
    Chat(ChatAction),
    Delete,
}

fn resolve(action: SwipeAction, room: &RoomSummary) -> Option<(IconName, Act)> {
    let pick = |want: fn(&ChatAction) -> bool| chat_actions(room).into_iter().find(|(_, _, a)| want(a)).map(|(_, icon, a)| (icon, Act::Chat(a)));
    match action {
        SwipeAction::None => None,
        SwipeAction::Archive => pick(|a| matches!(a, ChatAction::Archive(_))),
        SwipeAction::Pin => pick(|a| matches!(a, ChatAction::Pin(_))),
        SwipeAction::LowPriority => pick(|a| matches!(a, ChatAction::LowPriority(_))),
        SwipeAction::Mute => pick(|a| matches!(a, ChatAction::Mute(_))),
        SwipeAction::Unread => pick(|a| matches!(a, ChatAction::Unread(_))),
        SwipeAction::Delete => Some((if room.network.is_some() { IconName::Trash } else { IconName::LogOut }, Act::Delete)),
    }
}

/// Whether the action has already reached the chat, so a row still on the list is staying there.
fn applied(room: &RoomSummary, action: ChatAction) -> bool {
    match action {
        ChatAction::Archive(on) => room.archived == on,
        ChatAction::LowPriority(on) => room.low_priority == on,
        _ => true,
    }
}

/// The distance a release has to pass to act, for a row this wide.
pub(super) fn arm(width: f32) -> f32 {
    (width * ARM_SHARE).clamp(ARM_MIN, ARM_MAX)
}

/// Whether letting go at `dx` moving at `vel` (px/s) acts: past the arm point unless flung back,
/// or short of it but flicked outward.
pub(super) fn fires(dx: f32, vel: f32, arm: f32) -> bool {
    let along = vel * dx.signum();
    (dx.abs() >= arm && along > -FLICK_SPEED) || (dx.abs() >= FLICK_MIN && along >= FLICK_SPEED)
}

/// A pan that began on a row, until the finger lifts.
pub(super) struct Pan<T> {
    id: T,
    moved: (f32, f32),
    /// Undecided, then whether the row follows the finger or the pan is not a swipe.
    lock: Option<bool>,
    /// The row's sideways offset, negative to the left.
    pub dx: f32,
    samples: VecDeque<(Instant, f32)>,
}

/// The end of a sideways pan: where the row was and how fast it moved.
#[derive(Debug, PartialEq)]
pub(super) struct Release<T> {
    pub id: T,
    pub dx: f32,
    pub vel: f32,
    /// False for a cancelled touch, which never acts.
    pub ended: bool,
}

/// Advances a pan over a row by one scroll event. Nothing moves until the pan is `SWIPE_SLOP` long;
/// then it follows only a pan mostly sideways toward a side that has an action (`allowed`: left,
/// right), within `width`. A pan without a start (a mouse wheel, a fling's glide) never swipes.
/// A row still springing back is `caught` where it is drawn and follows at once.
#[allow(clippy::too_many_arguments, reason = "one step of one gesture; a struct would only rename them")]
pub(super) fn pan_step<T: Clone + PartialEq>(
    pan: &mut Option<Pan<T>>,
    id: &T,
    (dx, dy): (f32, f32),
    phase: TouchPhase,
    allowed: (bool, bool),
    width: f32,
    caught: Option<f32>,
    now: Instant,
) -> Option<Release<T>> {
    match phase {
        TouchPhase::Started => {
            *pan = Some(Pan { id: id.clone(), moved: (0., 0.), lock: caught.map(|_| true), dx: caught.unwrap_or(0.), samples: VecDeque::new() });
            return None;
        }
        TouchPhase::Moved => {}
        TouchPhase::Ended | TouchPhase::Cancelled => {
            let mut p = pan.take().filter(|p| p.lock == Some(true) && p.id == *id)?;
            p.samples.push_back((now, p.dx));
            let ended = phase == TouchPhase::Ended;
            return Some(Release { id: p.id, dx: p.dx, vel: if ended { gallery::speed(&p.samples) } else { 0. }, ended });
        }
    }
    let p = pan.as_mut().filter(|p| p.id == *id)?;
    let (lo, hi) = (if allowed.0 { -width } else { 0. }, if allowed.1 { width } else { 0. });
    match p.lock {
        None => {
            p.moved = (p.moved.0 + dx, p.moved.1 + dy);
            let (x, y) = p.moved;
            if x.hypot(y) < SWIPE_SLOP {
                return None;
            }
            let sideways = x.abs() > 1.5 * y.abs() && if x < 0. { allowed.0 } else { allowed.1 };
            p.lock = Some(sideways);
            // The row catches up with the finger instead of losing the slop.
            p.dx = if sideways { x.clamp(lo, hi) } else { 0. };
        }
        Some(true) => p.dx = (p.dx + dx).clamp(lo, hi),
        Some(false) => return None,
    }
    p.samples.push_back((now, p.dx));
    if p.samples.len() > 4 {
        p.samples.pop_front();
    }
    None
}

/// A row carried off the list, waiting for the chat to leave it.
struct Away {
    id: OwnedRoomId,
    /// The chat action sent, or `None` while the delete dialog asks.
    sent: Option<ChatAction>,
    /// The rows that were below it, top first.
    below: Vec<OwnedRoomId>,
    since: Instant,
}

#[derive(Default)]
pub(super) struct RowSwipe {
    pan: Option<Pan<OwnedRoomId>>,
    /// After the finger: the row springing back, or carried off and held there.
    glide: Option<(OwnedRoomId, Glide, bool)>,
    away: Option<Away>,
}

impl BeeperApp {
    /// The width a row moves within: the window on a phone, the list's column when wide.
    fn row_width(window: &Window) -> f32 {
        let w = f32::from(frame::viewport(window).width);
        if w < NARROW { w - Self::upright_rail(window) } else { LIST_W }
    }

    pub(super) fn row_pan(&mut self, id: &OwnedRoomId, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.rooms.iter().find(|r| r.id == *id) else { return };
        // A forum's row stands for many topics; a swipe acts on one chat.
        let forum_row = room.forum.as_ref().is_some_and(|f| self.forum_rows.contains_key(f));
        if forum_row || self.rows.away.is_some() && event.touch_phase == TouchPhase::Started {
            return;
        }
        let allowed = (resolve(self.settings.swipe_left, room).is_some(), resolve(self.settings.swipe_right, room).is_some());
        let width = Self::row_width(window);
        let delta = event.delta.pixel_delta(px(20.));
        let following = |pan: &Option<Pan<OwnedRoomId>>| pan.as_ref().filter(|p| p.lock == Some(true)).map(|p| p.dx);
        let before = following(&self.rows.pan);
        let now = Instant::now();
        // A row springing home is caught where it is drawn, as on iOS, instead of jumping under the finger.
        let caught = match (event.touch_phase, self.rows.glide.as_mut()) {
            (TouchPhase::Started, Some((on, g, false))) if on == id && g.running() => Some(g.step(now)),
            _ => None,
        };
        if caught.is_some() {
            self.rows.glide = None;
        }
        let released = pan_step(&mut self.rows.pan, id, (f32::from(delta.x), f32::from(delta.y)), event.touch_phase, allowed, width, caught, now);
        let after = following(&self.rows.pan);
        // Following the finger sideways, the pan is the row's: the list must not scroll with it.
        if after.is_some() || released.is_some() {
            cx.stop_propagation();
        }
        // A tick each way across the arm point, as iOS ticks when a full swipe arms and disarms.
        #[cfg(target_os = "android")]
        if let (Some(b), Some(a)) = (before, after)
            && (b.abs() >= arm(width)) != (a.abs() >= arm(width))
        {
            motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
        }
        if let Some(r) = released {
            self.row_released(r, width, cx);
        } else if before != after {
            cx.notify();
        }
    }

    fn row_released(&mut self, r: Release<OwnedRoomId>, width: f32, cx: &mut Context<Self>) {
        let Some(room) = self.rooms.iter().find(|room| room.id == r.id) else { return };
        let side = if r.dx < 0. { self.settings.swipe_left } else { self.settings.swipe_right };
        let act = (r.ended && fires(r.dx, r.vel, arm(width))).then(|| resolve(side, room)).flatten().map(|(_, act)| act);
        let away = r.dx.signum() * (width + GUTTER);
        match act {
            None => self.row_back(r.id, r.dx, r.vel, cx),
            Some(Act::Chat(ChatAction::Archive(true))) if room.pinned => {
                self.error = Some(t!("Pinned chats can't be archived").into());
                self.row_back(r.id, r.dx, r.vel, cx);
            }
            Some(Act::Chat(action)) => {
                let leaves = self.search.read(cx).value().trim().is_empty() && {
                    let mut after = room.clone();
                    match action {
                        ChatAction::Archive(on) => after.archived = on,
                        ChatAction::LowPriority(on) => after.low_priority = on,
                        _ => {}
                    }
                    in_view(self.view, room) && !in_view(self.view, &after)
                };
                if !self.chat_action(r.id.clone(), action, false, cx) || !leaves {
                    self.row_back(r.id, r.dx, r.vel, cx);
                } else {
                    self.row_away(r.id, Some(action), r.dx, away, r.vel, cx);
                }
            }
            Some(Act::Delete) => {
                self.row_away(r.id.clone(), None, r.dx, away, r.vel, cx);
                self.ask_leave(r.id, cx);
                self.row_settle(cx);
            }
        }
        cx.notify();
    }

    /// Springs the row home from `dx`, carrying the finger's speed; reduced motion snaps it there.
    fn row_back(&mut self, id: OwnedRoomId, dx: f32, vel: f32, cx: &App) {
        if self.rows.away.as_ref().is_some_and(|a| a.id == id) {
            self.rows.away = None;
        }
        if cx.reduce_motion() || dx == 0. {
            self.rows.glide = None;
            return;
        }
        let mut g = Glide::new(motion::SETTLE.0, 0.85, 0.5, dx);
        g.release(0., vel, Instant::now());
        self.rows.glide = Some((id, g, false));
    }

    fn row_away(&mut self, id: OwnedRoomId, sent: Option<ChatAction>, dx: f32, to: f32, vel: f32, cx: &mut Context<Self>) {
        let mut g = Glide::new(motion::THROW.0, motion::THROW.1, 0.5, dx);
        if cx.reduce_motion() {
            g.set(to);
        } else {
            g.release(to, vel, Instant::now());
        }
        let at = self.shown.iter().position(|&ix| self.rooms[ix].id == id);
        let below = at.map(|at| self.shown.iter().skip(at + 1).take(GAP_ROWS).map(|&ix| self.rooms[ix].id.clone()).collect()).unwrap_or_default();
        self.rows.glide = Some((id.clone(), g, true));
        let since = Instant::now();
        self.rows.away = Some(Away { id, sent, below, since });
        if sent.is_some() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(AWAY_FOR).await;
                this.update(cx, |this, cx| {
                    if this.rows.away.as_ref().is_some_and(|a| a.since == since) {
                        this.row_return(cx);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// After the list changed: a row carried off and now gone lets the rows below slide up into its
    /// place; one whose action landed but is still listed, or whose delete was not confirmed, returns.
    pub(super) fn row_settle(&mut self, cx: &App) {
        let Some(away) = self.rows.away.as_ref() else { return };
        let listed = |id: &OwnedRoomId| self.shown.iter().position(|&ix| self.rooms[ix].id == *id);
        let Some(at) = listed(&away.id) else {
            let away = self.rows.away.take().expect("checked above");
            self.rows.glide = None;
            if cx.reduce_motion() {
                return;
            }
            let start = away.below.first().and_then(listed);
            let shifted: Vec<_> = start
                .map(|start| away.below.iter().enumerate().take_while(|(k, id)| self.shown.get(start + k).is_some_and(|&ix| self.rooms[ix].id == **id)).map(|(_, id)| (id.clone(), -1)).collect())
                .unwrap_or_default();
            if !shifted.is_empty() {
                self.fly.start(slide::Reorder { shifted, entered: None }, ROOM_ROW_H);
            }
            return;
        };
        let room = &self.rooms[self.shown[at]];
        let stays = match away.sent {
            Some(action) => applied(room, action),
            None => !self.confirming(&away.id),
        };
        if stays {
            self.row_return(cx);
        }
    }

    /// The action failed: a row held off the list comes back.
    pub(super) fn row_return(&mut self, cx: &App) {
        if let Some(away) = self.rows.away.take() {
            let dx = self.rows.glide.as_mut().filter(|(id, ..)| *id == away.id).map_or(0., |(_, g, _)| g.step(Instant::now()));
            self.row_back(away.id, dx, 0., cx);
        }
    }

    /// The row's sideways offset now, and what shows behind it; asks for frames while it springs.
    pub(super) fn row_offset(&mut self, id: &OwnedRoomId, window: &mut Window) -> (f32, bool) {
        if let Some(p) = self.rows.pan.as_ref().filter(|p| p.id == *id && p.lock == Some(true)) {
            return (p.dx, p.dx.abs() >= arm(Self::row_width(window)));
        }
        let Some((_, g, lit)) = self.rows.glide.as_mut().filter(|(on, ..)| on == id) else { return (0., false) };
        let (dx, lit) = (g.step(Instant::now()), *lit);
        if g.running() {
            window.request_animation_frame();
        } else if !lit {
            self.rows.glide = None;
        }
        (dx, lit)
    }

    /// The colour and icon behind a row moved `dx`, lit when letting go would act.
    pub(super) fn row_backdrop(&self, room: &RoomSummary, dx: f32, armed: bool, window: &mut Window, cx: &mut App) -> Option<Div> {
        let side = if dx < 0. { self.settings.swipe_left } else { self.settings.swipe_right };
        let (icon, act) = resolve(side, room)?;
        let p = Palette::of(cx);
        // The accent is kept for what is addressed to the user.
        let fill = match act {
            Act::Delete => p.negative,
            Act::Chat(ChatAction::Archive(_)) => p.swipe_archive,
            Act::Chat(ChatAction::Unread(_)) => p.swipe_read,
            Act::Chat(ChatAction::Pin(_)) => p.swipe_pin,
            Act::Chat(ChatAction::Mute(_)) => p.swipe_mute,
            Act::Chat(ChatAction::LowPriority(_)) => p.swipe_later,
        };
        let reveal = (dx.abs() / (arm(Self::row_width(window)) * 0.6)).min(1.);
        let policy = if cx.reduce_motion() { Transition::new(Duration::ZERO) } else { Transition::new(LIGHT).easing(motion::EASE_OUT) };
        let lit = transition((SharedString::from(room.id.to_string()), "swipe-lit"), if armed { 1.0f32 } else { 0. }, policy, window, cx);
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(p.surface)
                // Faint while short of the arm point, so the coming action shows before it is chosen.
                .child(div().absolute().inset_0().bg(fill).opacity((FAINT * reveal).max(lit)))
                .child(
                    h_flex()
                        .absolute()
                        .inset_0()
                        .px(px(GUTTER + 8.))
                        .items_center()
                        .when(dx < 0., |el| el.justify_end())
                        .child(Icon::new(icon).size(px(20. + 2. * lit)).text_color(if lit > 0.5 { p.ground } else { p.muted }).opacity(reveal)),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Instant, Release, SwipeAction, TouchPhase, arm, fires, pan_step};
    use std::time::Duration;

    const BOTH: (bool, bool) = (true, true);

    fn pan(moves: &[(f32, f32)], allowed: (bool, bool)) -> (Option<super::Pan<u8>>, Instant) {
        let mut p = None;
        let mut t = Instant::now();
        pan_step(&mut p, &1, (0., 0.), TouchPhase::Started, allowed, 400., None, t);
        for &m in moves {
            t += Duration::from_millis(16);
            assert_eq!(pan_step(&mut p, &1, m, TouchPhase::Moved, allowed, 400., None, t), None);
        }
        (p, t)
    }

    #[test]
    fn axis_is_decided_once_past_the_slop() {
        let (p, _) = pan(&[(-4., 0.), (-4., 0.)], BOTH);
        assert_eq!(p.as_ref().unwrap().dx, 0., "under the slop nothing moves");
        let (p, _) = pan(&[(-6., 1.), (-6., 1.), (-10., 0.)], BOTH);
        assert_eq!(p.unwrap().dx, -22., "sideways: catches up with the finger, then follows 1:1");
        let (p, _) = pan(&[(2., -12.), (30., 0.)], BOTH);
        assert_eq!(p.unwrap().dx, 0., "a vertical pan never becomes a swipe");
    }

    #[test]
    fn a_side_without_an_action_does_not_move() {
        let (p, _) = pan(&[(12., 0.)], (true, false));
        assert_eq!(p.unwrap().dx, 0.);
        let (p, _) = pan(&[(-12., 0.), (40., 0.)], (true, false));
        assert_eq!(p.unwrap().dx, 0., "pulled back past home it stops there");
        let (p, _) = pan(&[(-12., 0.), (-900., 0.)], BOTH);
        assert_eq!(p.unwrap().dx, -400., "no further than the row's width");
    }

    #[test]
    fn release_reports_offset_and_cancel_never_acts() {
        let (mut p, t) = pan(&[(-12., 0.), (-20., 0.)], BOTH);
        let r = pan_step(&mut p, &1, (0., 0.), TouchPhase::Cancelled, BOTH, 400., None, t).unwrap();
        assert_eq!(r, Release { id: 1, dx: -32., vel: 0., ended: false });
        let (mut p, t) = pan(&[(0., 12.)], BOTH);
        assert_eq!(pan_step(&mut p, &1, (0., 0.), TouchPhase::Ended, BOTH, 400., None, t), None);
    }

    #[test]
    fn a_row_springing_home_is_caught_where_it_is() {
        let mut p = None;
        let t = Instant::now();
        pan_step(&mut p, &1, (0., 0.), TouchPhase::Started, BOTH, 400., Some(-80.), t);
        assert_eq!(p.as_ref().unwrap().dx, -80., "no jump under the finger");
        pan_step(&mut p, &1, (3., 0.), TouchPhase::Moved, BOTH, 400., None, t + Duration::from_millis(16));
        assert_eq!(p.unwrap().dx, -77., "follows at once, without a new slop");
    }

    #[test]
    fn threshold_and_flick_decide() {
        let a = arm(400.);
        assert!((a - 120.).abs() < 1e-3);
        assert_eq!((arm(100.), arm(2000.)), (72., 140.));
        assert!(fires(-a, 0., a) && fires(a + 1., 0., a));
        assert!(!fires(a - 1., 0., a), "short and slow springs back");
        assert!(fires(-40., -1200., a), "a flick acts short of the arm point");
        assert!(!fires(20., 1200., a), "too short to be a flick");
        assert!(!fires(a + 10., -1500., a), "flung back past home it does not act");
    }

    #[test]
    fn defaults() {
        assert_eq!((SwipeAction::left_default(), SwipeAction::right_default()), (SwipeAction::Archive, SwipeAction::Unread));
    }
}
