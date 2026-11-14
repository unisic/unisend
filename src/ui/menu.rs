//! A message's reactions and actions, after Signal: on touch a long press lifts the message over
//! a dimmed screen with a reaction bar above it and a list of actions under it (Signal Android's
//! reaction overlay); with a pointer, buttons beside the message and a menu at the pointer
//! (Signal Desktop). Measurements and timings are Signal's; `docs/design.md` says what they mean
//! here.

use std::time::{Duration, Instant};

use super::*;

/// The six reactions offered first, Signal's defaults in its order.
pub(super) const QUICK_REACTIONS: [&str; 6] = ["❤️", "👍", "👎", "😂", "😮", "😢"];

/// How long a press is held before it is a long press: GPUI's own for touch, used for a mouse too.
const LONG_PRESS: Duration = Duration::from_millis(500);
/// How far a pressed pointer may travel and still be a long press.
const PRESS_SLOP: f32 = 8.;

// The touch overlay.
/// Between the message, the reaction bar and the list of actions.
const OVERLAY_GAP: f32 = 12.;
/// A tall message's bar floats this far above the finger.
const BAR_OVER_FINGER: f32 = 48.;
/// A message shorter than this has the bar right above it, wherever it was touched.
const TIGHT_MESSAGE: f32 = 150.;
const BAR_H: f32 = 56.;
/// The bar's inner padding at each end.
const BAR_PAD: f32 = 8.;
const EMOJI: f32 = 28.;
/// The reaction under a sliding finger grows by half and rises.
const EMOJI_GROWN: f32 = 42.;
const EMOJI_RISE: f32 = 25.;
/// A finger that moves no further than this from where the overlay opened has only let go.
const DEAD_ZONE: f32 = 40.;
/// A reaction is chosen by sliding only between the bar's top and this far under the finger.
const SCRUB_BELOW_FINGER: f32 = 30.;
const ROW_H: f32 = 50.;
const LIST_W: f32 = 220.;

// The pointer's picker and menu.
const PICK: f32 = 40.;
const PICK_PAD_X: f32 = 7.;
const PICK_PAD_Y: f32 = 3.;
const PICK_EMOJI: f32 = 28.;
const PICK_GROWN: f32 = 42.;
const PICK_RISE: f32 = 16.;
/// Between the react button and the picker.
const PICK_OFFSET: f32 = 4.;
const POINTER_ROW_H: f32 = 40.;
const POINTER_LIST_W: f32 = 200.;
/// The buttons beside a message, as tall as they lay out; each takes pointer input over
/// `SIDE_HIT`, the dense-desktop minimum.
const SIDE_BUTTON: f32 = 28.;
const SIDE_HIT: f32 = 40.;

/// Paint order over the dimmed screen: the lifted message, then the bar and the list over it.
const LIFTED: usize = 1;
const OVER_LIFTED: usize = 2;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum MenuKind {
    /// The long-press overlay.
    Touch,
    /// The reactions alone, from the react button beside a message.
    Picker,
    /// The actions alone, from the more button or the right mouse button.
    List,
}

pub(super) struct MessageMenu {
    pub id: String,
    pub kind: MenuKind,
    /// Where the finger or pointer was, in window coordinates.
    at: Point<Pixels>,
    /// Touch: the message's bubble where it rests, without the lift.
    bubble: Bounds<Pixels>,
    opened: Instant,
    closing: Option<Instant>,
    /// Touch: the press that opened it is still down, and has left the dead zone.
    pressing: bool,
    escaped: bool,
    /// Touch: the reaction under the finger, the one before it, and when it changed.
    scrub: Option<usize>,
    scrub_prev: Option<usize>,
    scrub_at: Instant,
    /// Touch: where everything was put on the last frame.
    placed: Option<Placement>,
}

/// A mouse button held on a message, which becomes a long press unless it moves or lets go.
pub(super) struct Press {
    id: String,
    bubble: Bounds<Pixels>,
    at: Point<Pixels>,
    since: Instant,
}

/// What a message's list offers.
#[derive(Clone, PartialEq, Debug)]
enum Act {
    Reply,
    React,
    Retry,
    Link(String),
    Edit,
    Copy(String),
    Delete,
    Save,
}

impl Act {
    fn look(&self) -> (&'static str, IconName, &'static str) {
        match self {
            Act::Reply => ("act-reply", IconName::Reply, "Reply"),
            Act::React => ("act-react", IconName::Heart, "React"),
            Act::Retry => ("act-retry", IconName::RotateCw, "Try again"),
            Act::Link(_) => ("act-link", IconName::ExternalLink, "Open link"),
            Act::Edit => ("act-edit", IconName::Pencil, "Edit"),
            Act::Copy(_) => ("act-copy", IconName::Copy, "Copy text"),
            Act::Delete => ("act-delete", IconName::Trash, "Delete"),
            Act::Save => ("act-save", IconName::Download, "Save to gallery"),
        }
    }
}

/// The actions a message offers, in Signal's order: Android's for touch, Desktop's for a pointer.
/// `extra` puts Reply and React in the pointer's list, as Desktop does when its buttons beside the
/// message are not shown.
fn acts(m: &Msg, kind: MenuKind, extra: bool) -> Vec<Act> {
    // A message that has not reached the server can be sent again or dropped, nothing else.
    let sent = m.id.starts_with('$');
    let failed = matches!(m.delivery, Delivery::Failed(_));
    // An edit sends text: a picture, poll or place edited this way would turn into text.
    let editable = m.own && sent && m.media.is_none() && m.poll.is_none() && m.place.is_none() && !m.deleted;
    let link = m.link.as_ref().map(|link| link.url.clone()).or_else(|| urls(&m.body).into_iter().next().map(|r| m.body[r].to_owned()));
    let mut out = Vec::new();
    let saveable = crate::can_save() && sent && m.media.as_ref().is_some_and(|media| matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video));
    if kind == MenuKind::Touch {
        out.extend(sent.then_some(Act::Reply));
        out.extend(editable.then_some(Act::Edit));
        out.extend((!sent && failed).then_some(Act::Retry));
        out.extend(link.map(Act::Link));
        out.extend((!m.body.is_empty()).then(|| Act::Copy(m.body.clone())));
        out.extend(saveable.then_some(Act::Save));
        out.extend(m.own.then_some(Act::Delete));
    } else {
        if extra && sent {
            out.extend([Act::Reply, Act::React]);
        }
        out.extend(link.map(Act::Link));
        out.extend(editable.then_some(Act::Edit));
        out.extend((!m.body.is_empty()).then(|| Act::Copy(m.body.clone())));
        out.extend(saveable.then_some(Act::Save));
        out.extend(m.own.then_some(Act::Delete));
        out.extend((!sent && failed).then_some(Act::Retry));
    }
    out
}

/// The reactions in the bar: the six, then the user's own reaction when it is none of them, as
/// Signal puts an applied reaction that is not in its strip in the last place.
fn bar_keys(m: &Msg) -> Vec<String> {
    let mut keys: Vec<String> = QUICK_REACTIONS.iter().map(|k| (*k).to_owned()).collect();
    keys.extend(m.reactions.iter().find(|r| r.own && !QUICK_REACTIONS.iter().any(|k| same_emoji(k, &r.key))).map(|r| r.key.clone()));
    keys
}

/// One emoji with or without the emoji presentation selector: clients send "\u{2764}" and
/// "\u{2764}\u{FE0F}" for the same heart.
pub(super) fn same_emoji(a: &str, b: &str) -> bool {
    a.chars().filter(|&c| c != '\u{FE0F}').eq(b.chars().filter(|&c| c != '\u{FE0F}'))
}

/// Where the touch overlay puts things, in window pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Placement {
    /// How far the message moves so the bar and the list fit; negative is up.
    lift: f32,
    /// The bar's top-left corner and its number of reactions; `None` when it cannot react.
    bar: Option<(f32, f32, usize)>,
    list: (f32, f32),
}

fn bar_w(slots: usize) -> f32 {
    slots as f32 * TAP + 2. * BAR_PAD
}

/// Signal Android's placement on a phone (`ReactionOverlayPlacement.of`, narrow layout): the bar
/// above the message, the list under it, the message moved only as far as needed for both to fit.
/// A message too tall for that stays where it is with the bar at the top and the list at the
/// bottom over it, where Signal shrinks the message instead.
fn place(view: Size<f32>, (top, bottom): (f32, f32), bubble: Bounds<f32>, finger_y: f32, own: bool, slots: Option<usize>, list_h: f32) -> Placement {
    let roof = top + GUTTER;
    let floor = view.height - bottom - GUTTER;
    let bar_h = if slots.is_some() { BAR_H + OVERLAY_GAP } else { 0. };
    let (y, h) = (bubble.origin.y, bubble.size.height);
    let (bar_y, list_y, end_y) = if bar_h + h + OVERLAY_GAP + list_h <= floor - roof {
        let mut end_y = if y + h + OVERLAY_GAP + list_h > floor { floor - list_h - OVERLAY_GAP - h } else { y.max(roof) };
        // `barOffsetForTouch`: a short message has the bar right above it; a tall one has it above
        // the finger, which moved with the message.
        let finger = finger_y + end_y - y;
        let start = if h < TIGHT_MESSAGE { end_y } else { finger.min(end_y + h) - BAR_OVER_FINGER + OVERLAY_GAP };
        let mut bar_y = start - bar_h;
        if bar_y < roof {
            bar_y = roof;
            end_y = end_y.max(roof + bar_h);
        }
        (bar_y, end_y + h + OVERLAY_GAP, end_y)
    } else {
        (roof, floor - list_h, y)
    };
    let side = |w: f32| {
        let x = if own { bubble.origin.x + bubble.size.width - w } else { bubble.origin.x };
        x.clamp(GUTTER, (view.width - w - GUTTER).max(GUTTER))
    };
    Placement { lift: end_y - y, bar: slots.map(|n| (side(bar_w(n)), bar_y, n)), list: (side(LIST_W), list_y) }
}

/// The reaction under a finger sliding over the bar: within the bar's width, from its top to a
/// little under where the finger first came down (Signal's `ReactionScrubber.selectionAt`).
fn slot_at(placed: &Placement, at: Point<f32>, down_y: f32) -> Option<usize> {
    let (x, y, n) = placed.bar?;
    let dx = at.x - x - BAR_PAD;
    let inside = at.y > y && at.y < (down_y + SCRUB_BELOW_FINGER).max(y + BAR_H) && dx > 0. && dx < n as f32 * TAP;
    inside.then(|| ((dx / TAP) as usize).min(n - 1))
}

fn decelerate(t: f32) -> f32 {
    1. - (1. - t) * (1. - t)
}

fn accelerate_decelerate(t: f32) -> f32 {
    ((t + 1.) * std::f32::consts::PI).cos() / 2. + 0.5
}

/// Signal Desktop's `$ease-out-expo`, `cubic-bezier(0.19, 1, 0.22, 1)`.
fn ease_out_expo(t: f32) -> f32 {
    gpui_kit::base::Easing::CubicBezier { x1: 0.19, y1: 1., x2: 0.22, y2: 1. }.sample(t)
}

/// How far along an animation of `ms` that starts `delay` ms in is, `elapsed` ms in.
fn phase(elapsed: f32, delay: f32, ms: f32) -> f32 {
    ((elapsed - delay) / ms).clamp(0., 1.)
}

fn ms(since: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(since).as_secs_f32() * 1000.
}

impl MessageMenu {
    /// The touch overlay's reveal: Signal's 200 ms reveal, the bar 100 ms after, each reaction 10
    /// ms after the one before; its hide undoes whatever was shown in 150 ms.
    fn reveal(&self, now: Instant, delay: f32, ms_long: f32, curve: fn(f32) -> f32) -> f32 {
        match self.closing {
            Some(at) => curve(phase(ms(self.opened, at), delay, ms_long)) * (1. - decelerate(phase(ms(at, now), 0., 150.))),
            None => curve(phase(ms(self.opened, now), delay, ms_long)),
        }
    }

    fn finished(&self, now: Instant) -> bool {
        self.closing.is_some_and(|at| ms(at, now) >= 150.)
    }

    fn animating(&self, now: Instant) -> bool {
        self.closing.is_some() || ms(self.opened, now) < 400. || ms(self.scrub_at, now) < 200.
    }

    /// With reduced motion the message jumps instead of gliding.
    fn lift(&self, now: Instant, still: bool) -> f32 {
        let moved = match (still, self.closing) {
            (true, None) => 1.,
            (true, Some(_)) => 0.,
            (false, _) => self.reveal(now, 0., 200., accelerate_decelerate),
        };
        self.placed.map_or(0., |p| p.lift * moved)
    }

    /// How grown reaction `n` is, 0 to 1: Signal grows the one under the finger in 200 ms and lets
    /// the last one go back as fast.
    fn grown(&self, n: usize, now: Instant) -> f32 {
        let t = decelerate(phase(ms(self.scrub_at, now), 0., 200.));
        if self.scrub == Some(n) {
            t
        } else if self.scrub_prev == Some(n) {
            1. - t
        } else {
            0.
        }
    }
}

impl BeeperApp {
    pub(super) fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(|m| m.closing.is_none())
    }

    pub(super) fn open_menu(&mut self, id: String, kind: MenuKind, at: Point<Pixels>, bubble: Option<Bounds<Pixels>>, pressing: bool, cx: &mut Context<Self>) {
        if self.msgs.iter().all(|m| m.id != id || m.deleted) {
            return;
        }
        self.press = None;
        self.picker_hover = None;
        let now = Instant::now();
        #[cfg(target_os = "android")]
        if kind == MenuKind::Touch {
            super::motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Heavy);
        }
        self.menu = Some(MessageMenu {
            id,
            kind,
            at,
            bubble: bubble.unwrap_or(Bounds { origin: at, size: Size::default() }),
            opened: now,
            closing: None,
            pressing,
            escaped: false,
            scrub: None,
            scrub_prev: None,
            scrub_at: now,
            placed: None,
        });
        cx.notify();
    }

    /// The overlay fades away while what it was asked to do already happens; the pointer's lists
    /// go at once, as Desktop's do.
    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        match &mut self.menu {
            Some(m) if m.kind == MenuKind::Touch && m.closing.is_none() => {
                m.closing = Some(Instant::now());
                m.pressing = false;
            }
            Some(m) if m.kind == MenuKind::Touch => {}
            _ => self.menu = None,
        }
        cx.notify();
    }

    /// A mouse button went down on a message: a long press unless it moves or lets go first.
    fn press_start(&mut self, id: String, bubble: Bounds<Pixels>, at: Point<Pixels>, cx: &mut Context<Self>) {
        // One still fading out must not swallow the next press.
        if self.menu_open() {
            return;
        }
        let since = Instant::now();
        self.press = Some(Press { id, bubble, at, since });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LONG_PRESS).await;
            let _ = this.update(cx, |this, cx| {
                // Still the same press: one let go and pressed again has its own timer.
                if let Some(press) = this.press.take_if(|p| p.since == since) {
                    this.open_menu(press.id, MenuKind::Touch, press.at, Some(press.bubble), true, cx);
                }
            });
        })
        .detach();
    }

    fn pointer_moved(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.press.as_ref().is_some_and(|p| f32::from(at.x - p.at.x).hypot(f32::from(at.y - p.at.y)) > PRESS_SLOP) {
            self.press = None;
        }
        let Some(m) = self.menu.as_mut().filter(|m| m.pressing) else { return };
        // Signal's dead zone: until the finger leaves it nothing is chosen.
        let far = at - m.at;
        if !m.escaped && f32::from(far.x).abs() <= DEAD_ZONE && f32::from(far.y).abs() <= DEAD_ZONE {
            return;
        }
        m.escaped = true;
        let slot = m.placed.as_ref().and_then(|placed| slot_at(placed, point(f32::from(at.x), f32::from(at.y)), f32::from(m.at.y)));
        if slot != m.scrub {
            (m.scrub_prev, m.scrub, m.scrub_at) = (m.scrub, slot, Instant::now());
            #[cfg(target_os = "android")]
            if slot.is_some() {
                super::motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Selection);
            }
            cx.notify();
        }
    }

    /// The press that opened the overlay let go: on a reaction it reacts; having slid off the bar
    /// it closes; never having moved it leaves the overlay open for a tap.
    fn pointer_released(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.press = None;
        if !self.menu.as_ref().is_some_and(|m| m.pressing) {
            return;
        }
        self.pointer_moved(at, cx);
        let Some(m) = self.menu.as_mut() else { return };
        m.pressing = false;
        if !m.escaped {
            cx.notify();
            return;
        }
        let chosen = m.scrub.zip(self.msgs.iter().find(|msg| msg.id == m.id)).map(|(n, msg)| (msg.id.clone(), bar_keys(msg).swap_remove(n)));
        match chosen {
            Some((id, key)) => self.toggle_reaction(&id, &key, cx),
            None => self.close_menu(cx),
        }
    }

    /// Window-wide listeners for a press in progress: they follow the pointer wherever it goes.
    pub(super) fn press_tracker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event({
                    let weak = weak.clone();
                    move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase.bubble() {
                            let _ = weak.update(cx, |this, cx| this.pointer_moved(e.position, cx));
                        }
                    }
                });
                window.on_mouse_event({
                    let weak = weak.clone();
                    move |e: &MouseUpEvent, phase, _, cx| {
                        if phase.bubble() && e.button == MouseButton::Left {
                            let _ = weak.update(cx, |this, cx| this.pointer_released(e.position, cx));
                        }
                    }
                });
                window.on_mouse_event(move |e: &LongPressEvent, phase, _, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    let _ = weak.update(cx, |this, cx| match e.phase {
                        TouchPhase::Moved => this.pointer_moved(e.position, cx),
                        TouchPhase::Ended => this.pointer_released(e.position, cx),
                        TouchPhase::Cancelled if this.menu.as_ref().is_some_and(|m| m.pressing) => this.close_menu(cx),
                        _ => {}
                    });
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// A message's bubble, with what opens its overlay: a touch's long press and a held mouse
    /// button. While the overlay is up the bubble is lifted over it and taps on it only close it.
    pub(super) fn lifted(&self, bubble: Stateful<Div>, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let now = Instant::now();
        let still = cx.reduce_motion();
        let menu = self.menu.as_ref().filter(|m| m.id == id && m.kind == MenuKind::Touch && !m.finished(now));
        let lift = menu.map(|m| m.lift(now, still));
        let shield = menu.is_some_and(|m| m.closing.is_none());
        let weak = cx.entity().downgrade();
        let id = id.to_owned();
        let probe = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, cx| {
                let rest = Bounds { origin: point(bounds.origin.x, bounds.origin.y - px(lift.unwrap_or(0.))), size: bounds.size };
                if lift.is_some() {
                    let _ = weak.update(cx, |this, _| {
                        if let Some(m) = this.menu.as_mut().filter(|m| m.id == id) {
                            m.bubble = rest;
                        }
                    });
                }
                window.on_mouse_event({
                    let (weak, id, hitbox) = (weak.clone(), id.clone(), hitbox.clone());
                    move |e: &MouseDownEvent, phase, window, cx| {
                        if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                            let _ = weak.update(cx, |this, cx| this.press_start(id.clone(), rest, e.position, cx));
                        }
                    }
                });
                window.on_mouse_event(move |e: &LongPressEvent, phase, window, cx| {
                    if !phase.bubble() || e.phase != TouchPhase::Started || window.default_prevented() || !hitbox.is_hovered(window) {
                        return;
                    }
                    let Some(app) = weak.upgrade() else { return };
                    window.prevent_default();
                    window.capture_long_press(&app);
                    cx.stop_propagation();
                    app.update(cx, |this, cx| this.open_menu(id.clone(), MenuKind::Touch, e.start_position, Some(rest), true, cx));
                });
            },
        )
        .absolute()
        .inset_0();
        let bubble = bubble.relative().child(probe);
        match lift {
            Some(lift) => deferred(
                bubble.top(px(lift)).when(shield, |el| {
                    el.child(div().id("lifted-shield").absolute().inset_0().occlude().on_click(cx.listener(|this, _, _, cx| this.close_menu(cx))))
                }),
            )
            .with_priority(LIFTED)
            .into_any_element(),
            None => bubble.into_any_element(),
        }
    }

    /// A tap on a message does nothing of its own, as in Signal: a double tap likes it.
    pub(super) fn bubble_click(&mut self, id: &str, event: &ClickEvent, cx: &mut Context<Self>) {
        if event.click_count() == 2 && id.starts_with('$') {
            self.double_tap(id, event.position(), cx);
        }
    }

    /// Signal Desktop's buttons beside a message on a wide window: react, reply, more. They show
    /// while the pointer is over the message or one of their lists is open.
    pub(super) fn side_buttons(&self, m: &Msg, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let open = self.menu_open() && self.menu.as_ref().is_some_and(|menu| menu.id == m.id && menu.kind != MenuKind::Touch);
        let sent = m.id.starts_with('$');
        let button = |name: &'static str, icon: IconName| {
            div()
                .id(SharedString::from(format!("{}-{name}", m.id)))
                .size(px(SIDE_HIT))
                .my(px((SIDE_BUTTON - SIDE_HIT) / 2.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_color(p.faint)
                .cursor_pointer()
                .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.text_color(p.text)))
                .child(Icon::new(icon).size_5())
        };
        let (react, reply, more) = (m.id.clone(), m.id.clone(), m.id.clone());
        h_flex()
            .self_center()
            .flex_none()
            .when(m.own, |el| el.flex_row_reverse())
            .opacity(if open { 1. } else { 0. })
            .group_hover("message", |el| el.opacity(1.))
            .when(sent, |el| {
                el.child(button("react", IconName::Heart).on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    this.open_menu(react.clone(), MenuKind::Picker, e.position(), None, false, cx)
                })))
                .child(button("reply", IconName::Reply).on_click(cx.listener(move |this, _, window, cx| {
                    this.start_compose(Compose::Reply(reply.clone()), window, cx)
                })))
            })
            .child(button("more", IconName::Ellipsis).on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                this.open_menu(more.clone(), MenuKind::List, e.position(), None, false, cx)
            })))
    }

    fn act(&mut self, id: &str, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::Reply => self.start_compose(Compose::Reply(id.to_owned()), window, cx),
            Act::Edit => self.start_compose(Compose::Edit(id.to_owned()), window, cx),
            Act::React => {
                let at = self.menu.as_ref().map_or_else(Point::default, |m| m.at);
                self.open_menu(id.to_owned(), MenuKind::Picker, at, None, false, cx);
                return;
            }
            Act::Retry => {
                if let Some(room) = self.open.clone() {
                    self.send(Cmd::Retry { room, target: id.to_owned() }, cx);
                }
            }
            Act::Delete => {
                if let Some(room) = self.open.clone() {
                    self.send(Cmd::Delete { room, target: id.to_owned() }, cx);
                }
            }
            Act::Link(url) => crate::open_link(cx, &url),
            Act::Copy(body) => crate::copy(cx, body),
            Act::Save => self.export_message(id, crate::backend::Export::Save, cx),
        }
        self.close_menu(cx);
    }

    fn act_rows(&self, id: &str, acts: Vec<Act>, h: f32, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let p = Palette::of(cx);
        acts.into_iter()
            .map(|act| {
                let (name, icon, label) = act.look();
                let ink = if act == Act::Delete { p.negative } else { p.text };
                let id = id.to_owned();
                h_flex()
                    .id(name)
                    .h(px(h))
                    .px(px(GUTTER))
                    .gap(px(GUTTER))
                    .text_size(px(16.))
                    .text_color(ink)
                    .cursor_pointer()
                    .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.text.opacity(0.1))))
                    .active(|el| el.bg(p.text.opacity(0.1)))
                    .child(Icon::new(icon).size_5())
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| this.act(&id, act.clone(), window, cx)))
            })
            .collect()
    }

    /// The reactions and actions of the message the menu is for, or nothing.
    pub(super) fn render_message_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let now = Instant::now();
        if self.menu.as_ref().is_some_and(|m| m.finished(now) || self.msgs.iter().all(|msg| msg.id != m.id || msg.deleted)) {
            self.menu = None;
        }
        let menu = self.menu.as_ref()?;
        if menu.animating(now) {
            window.request_animation_frame();
        }
        match menu.kind {
            MenuKind::Touch => self.render_overlay(now, window, cx),
            MenuKind::Picker => self.render_picker(now, window, cx),
            MenuKind::List => self.render_list(window, cx),
        }
    }

    fn render_overlay(&mut self, now: Instant, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let still = cx.reduce_motion();
        let view = window.viewport_size();
        let menu = self.menu.as_ref()?;
        let m = self.msgs.iter().find(|m| m.id == menu.id)?;
        let id = m.id.clone();
        let keys = bar_keys(m);
        let sent = id.starts_with('$');
        let acts = acts(m, MenuKind::Touch, false);
        let mine: Vec<bool> = keys.iter().map(|k| m.reactions.iter().any(|r| r.own && same_emoji(&r.key, k))).collect();
        let b = menu.bubble;
        let bubble = Bounds { origin: point(f32::from(b.origin.x), f32::from(b.origin.y)), size: size(f32::from(b.size.width), f32::from(b.size.height)) };
        let placed = place(
            size(f32::from(view.width), f32::from(view.height)),
            crate::insets(),
            bubble,
            f32::from(menu.at.y),
            m.own,
            sent.then_some(keys.len()),
            ROW_H * acts.len() as f32 + 16.,
        );
        let menu = self.menu.as_mut()?;
        menu.placed = Some(placed);
        let menu = self.menu.as_ref()?;
        let open = menu.closing.is_none();
        let shade = menu.reveal(now, 0., 1., |_| 1.);
        let rise = |n: f32| if still { 0. } else { EMOJI_RISE * (1. - menu.reveal(now, 100. + 10. * n, 200., decelerate)) };
        let bar = placed.bar.map(|(x, y, _)| {
            let slots = keys.iter().enumerate().map(|(n, key)| {
                let (id, label, key) = (id.clone(), key.clone(), key.clone());
                let grown = if still { 0. } else { menu.grown(n, now) };
                div()
                    .id(("bar-reaction", n))
                    .relative()
                    .size(px(TAP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .when(mine[n], |el| el.border_2().border_color(p.text.opacity(menu.reveal(now, 0., 400., decelerate))))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key, cx)))
                    .child(
                        div()
                            .relative()
                            .top(px(rise(n as f32) - EMOJI_RISE * grown))
                            .opacity(menu.reveal(now, 100. + 10. * n as f32, 200., decelerate))
                            .text_size(px(EMOJI + (EMOJI_GROWN - EMOJI) * grown))
                            .line_height(px(EMOJI_GROWN))
                            .child(label),
                    )
            });
            deferred(
                div().absolute().left(px(x)).top(px(y)).child(
                    h_flex()
                        .id("reaction-bar")
                        .h(px(BAR_H))
                        .px(px(BAR_PAD))
                        .rounded_full()
                        .bg(p.lifted.opacity(menu.reveal(now, 100., 200., decelerate)))
                        .when(open, |el| el.occlude())
                        .children(slots),
                ),
            )
            .with_priority(OVER_LIFTED)
        });
        let rows = self.act_rows(&id, acts, ROW_H, cx);
        let (lx, ly) = placed.list;
        // Signal's list waits 150 ms for the message to settle, then fades in over 200 ms.
        let list = deferred(
            div().absolute().left(px(lx)).top(px(ly)).child(
                v_flex()
                    .id("message-actions")
                    .w(px(LIST_W))
                    .py(px(8.))
                    .rounded(px(RADIUS))
                    .bg(p.lifted)
                    .opacity(menu.reveal(now, 150., 200., decelerate))
                    .when(open, |el| el.occlude())
                    .children(rows),
            ),
        )
        .with_priority(OVER_LIFTED);
        Some(
            div()
                .id("message-overlay")
                .absolute()
                .inset_0()
                .bg(p.ground.opacity(0.85 * shade))
                .when(open, |el| {
                    el.occlude().on_click(cx.listener(|this, _, _, cx| {
                        // The press that opened it lets go here too; only a tap afterwards closes it.
                        if !this.menu.as_ref().is_some_and(|m| m.pressing) {
                            this.close_menu(cx);
                        }
                    }))
                })
                .children(bar)
                .child(list)
                .into_any_element(),
        )
    }

    /// Signal Desktop's reaction picker: a row of reactions above the react button.
    fn render_picker(&mut self, now: Instant, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let still = cx.reduce_motion();
        let view = window.viewport_size();
        let menu = self.menu.as_ref()?;
        let m = self.msgs.iter().find(|m| m.id == menu.id)?;
        let (id, keys) = (m.id.clone(), bar_keys(m));
        let w = keys.len() as f32 * PICK + 2. * PICK_PAD_X;
        let h = PICK + 2. * PICK_PAD_Y;
        let (top, bottom) = crate::insets();
        let at = (f32::from(menu.at.x), f32::from(menu.at.y));
        let x = (at.0 - w / 2.).clamp(GUTTER, (f32::from(view.width) - w - GUTTER).max(GUTTER));
        let above = at.1 - SIDE_BUTTON / 2. - PICK_OFFSET - h;
        let y = if above >= top + GUTTER { above } else { (at.1 + SIDE_BUTTON / 2. + PICK_OFFSET).min(f32::from(view.height) - bottom - h) };
        let t = ms(menu.opened, now);
        let hover = self.picker_hover;
        let buttons: Vec<_> = keys.iter().enumerate().map(|(n, key)| {
            let mine = m.reactions.iter().any(|r| r.own && same_emoji(&r.key, key));
            let (id, label, key) = (id.clone(), key.clone(), key.clone());
            let shown = if still { 1. } else { ease_out_expo(phase(t, 10. * n as f32, 400.)) };
            let grow = (!still && hover == Some(n)) as u8 as f32;
            let grow = transition((("pick", n), "grow"), grow, Transition::new(Duration::from_millis(120)).easing(motion::EASE_OUT), window, cx);
            div()
                .id(("picker-reaction", n))
                .relative()
                .top(px(24. * (1. - shown) - PICK_RISE * grow))
                .opacity(shown)
                .size(px(PICK))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .when(mine, |el| el.border_2().border_color(p.text))
                .text_size(px(PICK_EMOJI + (PICK_GROWN - PICK_EMOJI) * grow))
                .line_height(px(PICK_GROWN))
                .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                    let next = if *over { Some(n) } else { this.picker_hover.filter(|&h| h != n) };
                    if next != this.picker_hover {
                        this.picker_hover = next;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key, cx)))
                .child(label)
        }).collect();
        let picker = div().absolute().left(px(x)).top(px(y)).child(
            h_flex()
                .id("reaction-picker")
                .px(px(PICK_PAD_X))
                .py(px(PICK_PAD_Y))
                .rounded_full()
                .bg(p.lifted)
                .opacity(if still { 1. } else { ease_out_expo(phase(t, 0., 400.)) })
                .occlude()
                .children(buttons),
        );
        Some(self.barrier(picker.into_any_element(), cx))
    }

    /// Signal Desktop's message menu, at the pointer.
    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let view = window.viewport_size();
        let narrow = view.width < px(NARROW);
        let menu = self.menu.as_ref()?;
        let m = self.msgs.iter().find(|m| m.id == menu.id)?;
        let id = m.id.clone();
        let acts = acts(m, MenuKind::List, narrow);
        let h = POINTER_ROW_H * acts.len() as f32 + 12.;
        let (top, bottom) = crate::insets();
        let (ax, ay) = (f32::from(menu.at.x), f32::from(menu.at.y));
        let (vw, vh) = (f32::from(view.width), f32::from(view.height));
        let x = if ax + POINTER_LIST_W <= vw - GUTTER { ax } else { ax - POINTER_LIST_W };
        let y = if ay + h <= vh - bottom - GUTTER { ay } else { ay - h };
        let x = x.clamp(GUTTER, (vw - POINTER_LIST_W - GUTTER).max(GUTTER));
        let y = y.clamp(top + GUTTER, (vh - bottom - h - GUTTER).max(top + GUTTER));
        let rows = self.act_rows(&id, acts, POINTER_ROW_H, cx);
        let list = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .child(v_flex().id("message-menu").w(px(POINTER_LIST_W)).py(px(6.)).rounded(px(14.)).bg(p.lifted).occlude().children(rows))
            .with_animation(SharedString::from(format!("menu-list-{id}")), Animation::new(motion::ENTER).with_easing(motion::ease_out), move |el, t| {
                el.top(px(y + 4. * (1. - t))).opacity(t)
            });
        Some(self.barrier(list.into_any_element(), cx))
    }

    /// The pointer's lists close on a click or a right click anywhere else; nothing is dimmed.
    fn barrier(&self, content: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("message-menu-barrier")
            .absolute()
            .inset_0()
            .occlude()
            .on_click(cx.listener(|this, _, _, cx| this.close_menu(cx)))
            .on_aux_click(cx.listener(|this, _, _, cx| this.close_menu(cx)))
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{BAR_H, OVERLAY_GAP, Placement, TAP, ease_out_expo, place, same_emoji, slot_at};
    use gpui_kit::{Bounds, point, size};

    #[test]
    fn heart_with_and_without_selector_is_one_emoji() {
        assert!(same_emoji("\u{2764}", "\u{2764}\u{FE0F}"));
        assert!(same_emoji("\u{1F44D}", "\u{1F44D}"));
        assert!(!same_emoji("\u{2764}", "\u{1F44D}"));
    }

    const PHONE: (f32, f32) = (400., 900.);
    const LIST_H: f32 = 216.;

    fn at(y: f32, h: f32, own: bool) -> Placement {
        let x = if own { 200. } else { 16. };
        place(size(PHONE.0, PHONE.1), (0., 0.), Bounds { origin: point(x, y), size: size(184., h) }, y + h / 2., own, Some(6), LIST_H)
    }

    fn inside(p: &Placement, y: f32, h: f32) {
        let (bx, by, n) = p.bar.unwrap();
        assert!(bx >= 16. && bx + n as f32 * TAP + 16. <= PHONE.0 - 16., "bar inside the width: {p:?}");
        assert!(by >= 16., "bar under the roof: {p:?}");
        assert!(p.list.1 + LIST_H <= PHONE.1 - 16., "list above the floor: {p:?}");
        let top = y + p.lift;
        if h + BAR_H + LIST_H + 2. * OVERLAY_GAP <= PHONE.1 - 32. {
            assert!(by + BAR_H <= top, "bar above the message: {p:?}");
            assert!(p.list.1 >= top + h, "list under the message: {p:?}");
        }
    }

    #[test]
    fn the_overlay_fits_wherever_the_message_is() {
        let mid = at(400., 60., false);
        assert_eq!(mid.lift, 0., "room everywhere: nothing moves");
        assert_eq!(mid.bar.unwrap().1, 400. - OVERLAY_GAP - BAR_H, "a short message has the bar right above it");
        inside(&mid, 400., 60.);
        let top = at(20., 60., false);
        assert!(top.lift > 0., "at the top it moves down for the bar");
        inside(&top, 20., 60.);
        let bottom = at(820., 60., true);
        assert!(bottom.lift < 0., "at the bottom it moves up for the list");
        inside(&bottom, 820., 60.);
        let tall = at(-300., 1400., false);
        assert_eq!(tall.lift, 0., "taller than the screen: it stays");
        inside(&tall, -300., 1400.);
        let narrow = place(size(360., 640.), (24., 48.), Bounds { origin: point(160., 500.), size: size(184., 40.) }, 520., true, Some(7), LIST_H);
        let (bx, _, n) = narrow.bar.unwrap();
        assert!(bx >= 16. && bx + n as f32 * TAP + 16. <= 344., "{narrow:?}");
    }

    #[test]
    fn a_sliding_finger_picks_the_reaction_under_it() {
        let p = at(400., 60., false);
        let (x, y, _) = p.bar.unwrap();
        let down = 430.;
        assert_eq!(slot_at(&p, point(x + 8. + TAP * 2.5, y + 20.), down), Some(2));
        assert_eq!(slot_at(&p, point(x + 8. + TAP * 2.5, down + 20.), down), Some(2), "a little under the finger still counts");
        assert_eq!(slot_at(&p, point(x + 8. + TAP * 2.5, down + 40.), down), None, "far under it does not");
        assert_eq!(slot_at(&p, point(x - 4., y + 20.), down), None);
        assert_eq!(slot_at(&p, point(x + 8. + TAP * 2.5, y - 1.), down), None);
    }

    #[test]
    fn the_expo_curve_starts_fast_and_lands_on_its_ends() {
        assert!(ease_out_expo(0.).abs() < 1e-3);
        assert!((ease_out_expo(1.) - 1.).abs() < 1e-3);
        assert!(ease_out_expo(0.2) > 0.7, "most of the way there in a fifth of the time");
    }
}
