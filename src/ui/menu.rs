//! A message's reactions and actions, after Signal: on touch a long press lifts the message over
//! a dimmed screen with a reaction bar above it and a list of actions under it (Signal Android's
//! reaction overlay), where the finger that pressed can slide onto a reaction or an action and
//! lift to pick it, as in an iOS context menu; with a pointer, buttons beside the message and a
//! menu at the pointer (Signal Desktop). Measurements and timings are Signal's; `docs/design.md`
//! says what they mean here.

use std::time::{Duration, Instant};

use super::*;
use crate::backend::Features;
use crate::i18n::t;

/// The six reactions offered first, Signal's defaults in its order.
pub(super) const QUICK_REACTIONS: [&str; 6] = ["❤️", "👍", "👎", "😂", "😮", "😢"];

/// How long a press is held before it is a long press: GPUI's own for touch, used for a mouse too.
const LONG_PRESS: Duration = Duration::from_millis(500);
/// Lines the Select text field grows to before it scrolls.
const SELECT_TEXT_ROWS: usize = 14;
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
/// The action list slides this far out of the bubble while it fades in.
const LIST_RISE: f32 = 8.;
/// A finger that moves no further than this from where the overlay opened has only let go.
const DEAD_ZONE: f32 = 40.;
/// A reaction is chosen by sliding only between the bar's top and this far under the finger.
const SCRUB_BELOW_FINGER: f32 = 30.;
const ROW_H: f32 = 50.;
/// Above the first action and under the last.
const LIST_PAD: f32 = 8.;
/// At 100% text; the lists widen with the text size so "Delete for everyone" fits.
const LIST_W: f32 = 232.;

// The pointer's picker and menu.
const PICK: f32 = 40.;
const PICK_PAD_X: f32 = 7.;
const PICK_PAD_Y: f32 = 3.;
const PICK_EMOJI: f32 = 28.;
const PICK_GROWN: f32 = 42.;
const PICK_RISE: f32 = 16.;
/// Between the react button and the picker.
const PICK_OFFSET: f32 = 4.;
pub(super) const POINTER_ROW_H: f32 = 40.;
const POINTER_LIST_W: f32 = 232.;
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
    /// Touch: the action under the finger, which letting go picks.
    pick: Option<usize>,
    /// Touch: where everything was put on the last frame.
    placed: Option<Placement>,
}

/// A mouse button held on a message, which becomes a long press unless it moves or lets go.
pub(super) struct Press {
    id: String,
    at: Point<Pixels>,
    since: Instant,
}

/// What a message's list offers.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Act {
    Reply,
    Thread,
    React,
    Retry,
    Link(String),
    CopyLink(String),
    Edit,
    Copy(String),
    /// The text in a sheet, where any part of it can be selected and copied.
    SelectText(String),
    Delete,
    EndPoll,
    Save,
    Forward,
    Select,
    Pin,
    Unpin,
    Reactions,
    Info,
}

impl Act {
    fn look(&self) -> (&'static str, IconName, &'static str) {
        match self {
            Act::Reply => ("act-reply", IconName::Reply, t!("Reply")),
            Act::Thread => ("act-thread", IconName::Reply, t!("Reply in thread")),
            Act::React => ("act-react", IconName::Heart, t!("React")),
            Act::Retry => ("act-retry", IconName::RotateCw, t!("Try again")),
            Act::Link(_) => ("act-link", IconName::ExternalLink, t!("Open link")),
            Act::CopyLink(_) => ("act-copy-link", IconName::Link, t!("Copy link")),
            Act::Edit => ("act-edit", IconName::Pencil, t!("Edit")),
            Act::Copy(_) => ("act-copy", IconName::Copy, t!("Copy text")),
            Act::SelectText(_) => ("act-select-text", IconName::TextCursor, t!("Select text")),
            Act::Delete => ("act-delete", IconName::Trash, t!("Delete")),
            Act::EndPoll => ("act-end-poll", IconName::Check, t!("End poll")),
            Act::Save => ("act-save", IconName::Download, if cfg!(target_os = "android") { t!("Save to gallery") } else { t!("Save") }),
            Act::Forward => ("act-forward", IconName::Forward, t!("Forward")),
            Act::Select => ("act-select", IconName::ListChecks, t!("Select multiple")),
            Act::Pin => ("act-pin", IconName::Pin, t!("Pin")),
            Act::Unpin => ("act-unpin", IconName::PinOff, t!("Unpin")),
            Act::Reactions => ("act-reactions", IconName::FaceSlightlySmiling, t!("Reactions")),
            Act::Info => ("act-info", IconName::Info, t!("Info")),
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
    let ends = m.own && sent && !m.deleted && m.poll.as_ref().is_some_and(|p| p.ended.is_none() && p.unstable);
    let mut out = Vec::new();
    let saveable = crate::can_save() && sent && m.media.as_ref().is_some_and(|media| !media.once && matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video));
    // Reply and React head the list only where no buttons beside the message offer them; the touch
    // overlay has its own reactions. Past them, every menu lists the same acts in the same order.
    if sent && (kind == MenuKind::Touch || extra) {
        out.push(Act::Reply);
    }
    if kind != MenuKind::Touch && extra && sent {
        out.push(Act::React);
    }
    out.extend(editable.then_some(Act::Edit));
    out.extend((!sent && failed).then_some(Act::Retry));
    out.extend(link.into_iter().flat_map(|url| [Act::Link(url.clone()), Act::CopyLink(url)]));
    out.extend((!m.body.is_empty()).then(|| Act::Copy(m.body.clone())));
    // A mouse selects in the bubble itself, as in a browser.
    out.extend((!m.body.is_empty() && kind == MenuKind::Touch).then(|| Act::SelectText(m.body.clone())));
    out.extend(ends.then_some(Act::EndPoll));
    out.extend(saveable.then_some(Act::Save));
    out.extend(more_acts(m));
    // One that never left is only dropped; one that did asks whether for everyone or only here.
    out.extend((sent || m.own).then_some(Act::Delete));
    out
}

/// What the chat's network takes of `acts`: Reply, React, and an edit only within its time on the
/// network.
fn taken(mut acts: Vec<Act>, m: &Msg, caps: &Features) -> Vec<Act> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    acts.retain(|act| match act {
        Act::Reply => caps.reply >= 1,
        Act::Thread => caps.thread >= 1,
        Act::React => caps.reaction >= 1,
        Act::Edit => caps.editable(m.ts, now),
        _ => true,
    });
    acts
}

/// Reply in thread, Forward, Select, Reactions (when it has any) and Info, for a message that
/// reached the server.
fn more_acts(m: &Msg) -> Vec<Act> {
    if !m.id.starts_with('$') {
        return Vec::new();
    }
    let mut out = vec![Act::Thread];
    out.extend(super::forward::forwardable(m).then_some(Act::Forward));
    out.push(Act::Select);
    // Who reacted, without a long press on a badge, where a slip toggles a reaction.
    out.extend((!m.reactions.is_empty()).then_some(Act::Reactions));
    out.push(Act::Info);
    out
}

/// The reactions in the bar: the six the chat takes (where it takes none of them, the first six
/// it takes), then the user's own reaction when it is none of them, as Signal puts an applied
/// reaction that is not in its strip in the last place.
fn bar_keys(m: &Msg, caps: &Features, quick: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = quick.iter().filter(|k| caps.reacts_with(k)).cloned().collect();
    if keys.is_empty() {
        keys = caps.allowed_reactions.iter().flatten().take(QUICK_REACTIONS.len()).cloned().collect();
    }
    keys.extend(m.reactions.iter().find(|r| r.own && !keys.iter().any(|k| same_emoji(k, &r.key))).map(|r| r.key.clone()));
    keys
}

/// The bar's six as the user set them in Settings, else Signal's.
pub(super) fn quick(settings: &crate::Settings) -> Vec<String> {
    if settings.reactions.len() == QUICK_REACTIONS.len() { settings.reactions.clone() } else { QUICK_REACTIONS.map(str::to_owned).to_vec() }
}

/// Sets slot `n` of the bar's six to `emoji`, or the double tap's at `n` 6. An emoji already in
/// the six trades places with the one it replaces, so none shows twice.
pub(super) fn set_quick(settings: &mut crate::Settings, n: usize, emoji: &str) {
    if n == QUICK_REACTIONS.len() {
        settings.double_tap = (!same_emoji(emoji, super::LIKE)).then(|| emoji.to_owned());
        return;
    }
    let mut six = quick(settings);
    if n >= six.len() {
        return;
    }
    match six.iter().position(|k| same_emoji(k, emoji)) {
        Some(j) => six.swap(j, n),
        None => six[n] = emoji.to_owned(),
    }
    settings.reactions = if six == QUICK_REACTIONS { Vec::new() } else { six };
}

/// The bar ends with a + for every other emoji, unless the chat takes only what the bar shows.
fn more_offered(keys: &[String], caps: &Features) -> bool {
    caps.allowed_reactions.as_ref().is_none_or(|list| list.iter().any(|a| !keys.iter().any(|k| same_emoji(k, a))))
}

/// A reaction key as shown: any emoji whole and in its emoji form ("\u{2764}" is a white text
/// heart in Inter 4), someone's long text cut short.
pub(super) fn clip_key(key: &str) -> String {
    emojis::get(key).map_or_else(|| clip_chars(key, 16), |e| e.as_str().to_owned())
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
    /// How many actions the list holds, for a finger sliding onto one.
    rows: usize,
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
    Placement { lift: end_y - y, bar: slots.map(|n| (side(bar_w(n)), bar_y, n)), list: (side(f32::from(fs(LIST_W))), list_y), rows: 0 }
}

/// The reaction under a finger sliding over the bar: within the bar's width, from its top to a
/// little under where the finger first came down (Signal's `ReactionScrubber.selectionAt`).
fn slot_at(placed: &Placement, at: Point<f32>, down_y: f32) -> Option<usize> {
    let (x, y, n) = placed.bar?;
    let dx = at.x - x - BAR_PAD;
    let inside = at.y > y && at.y < (down_y + SCRUB_BELOW_FINGER).max(y + BAR_H) && dx > 0. && dx < n as f32 * TAP;
    inside.then(|| ((dx / TAP) as usize).min(n - 1))
}

/// The action under a finger slid onto the list, as iOS lets a held finger pick from a context menu.
fn row_at(placed: &Placement, at: Point<f32>) -> Option<usize> {
    let (x, y) = placed.list;
    let (dx, dy) = (at.x - x, at.y - y - LIST_PAD);
    let inside = dx > 0. && dx < f32::from(fs(LIST_W)) && dy > 0. && dy < ROW_H * placed.rows as f32;
    inside.then(|| ((dy / ROW_H) as usize).min(placed.rows.saturating_sub(1)))
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

    fn animating(&self, now: Instant, still: bool) -> bool {
        self.closing.is_some() || (!still && (ms(self.opened, now) < 400. || ms(self.scrub_at, now) < 200.))
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
    /// Pin or Unpin before Select, for a message that reached the server where the user's power
    /// level allows `m.room.pinned_events`; no Reply in thread inside a thread.
    fn with_pin(&self, mut acts: Vec<Act>, m: &Msg) -> Vec<Act> {
        if self.thread.is_some() {
            acts.retain(|a| *a != Act::Thread);
        }
        if let Some(pinned) = self.pin_offer(&m.id).filter(|_| m.id.starts_with('$') && !m.deleted) {
            let at = acts.iter().position(|a| *a == Act::Select).unwrap_or(acts.len());
            acts.insert(at, if pinned { Act::Unpin } else { Act::Pin });
        }
        acts
    }

    /// The user's last message, when the chat's network still takes an edit to it: what Up in an
    /// empty composer edits.
    pub(super) fn last_editable(&self) -> Option<String> {
        let m = self.msgs.iter().rev().find(|m| m.own && !m.deleted)?;
        taken(acts(m, MenuKind::List, true), m, self.caps_for(&m.id)).contains(&Act::Edit).then(|| m.id.clone())
    }

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
            super::motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Medium);
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
            pick: None,
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
    fn press_start(&mut self, id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        // One still fading out must not swallow the next press.
        if self.menu_open() {
            return;
        }
        let since = Instant::now();
        self.press = Some(Press { id, at, since });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LONG_PRESS).await;
            let _ = this.update(cx, |this, cx| {
                // Still the same press: one let go and pressed again has its own timer.
                if let Some(press) = this.press.take_if(|p| p.since == since) {
                    this.bubble_press = None;
                    // A held mouse button: the list opens where it is, as for the right button.
                    this.open_menu(press.id, MenuKind::List, press.at, None, false, cx);
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
        let here = point(f32::from(at.x), f32::from(at.y));
        let pick = m.placed.as_ref().and_then(|placed| row_at(placed, here));
        if pick != m.pick {
            m.pick = pick;
            #[cfg(target_os = "android")]
            if pick.is_some() {
                super::motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Selection);
            }
            cx.notify();
        }
        let slot = m.placed.as_ref().and_then(|placed| slot_at(placed, here, f32::from(m.at.y)));
        if slot != m.scrub {
            (m.scrub_prev, m.scrub, m.scrub_at) = (m.scrub, slot, Instant::now());
            #[cfg(target_os = "android")]
            if slot.is_some() {
                super::motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Selection);
            }
            cx.notify();
        }
    }

    /// The press that opened the overlay let go: on a reaction it reacts, on an action it does it;
    /// anywhere else, as on iOS, it leaves the overlay open for a tap.
    fn pointer_released(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
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
        let (scrub, pick, menu_id) = (m.scrub, m.pick.take(), m.id.clone());
        if let Some(n) = pick
            && let Some(msg) = self.msgs.iter().find(|msg| msg.id == menu_id)
        {
            let acts = self.with_pin(taken(acts(msg, MenuKind::Touch, false), msg, self.caps_for(&msg.id)), msg);
            if let Some(act) = acts.into_iter().nth(n) {
                return self.act(&menu_id, act, window, cx);
            }
        }
        let chosen = scrub.zip(self.msgs.iter().find(|msg| msg.id == menu_id)).map(|(n, msg)| (msg.id.clone(), bar_keys(msg, self.caps_for(&msg.id), &quick(&self.settings)).get(n).cloned()));
        match chosen {
            Some((id, Some(key))) => self.toggle_reaction(&id, &key, cx),
            Some((id, None)) => {
                self.close_menu(cx);
                self.emoji_open = Some(super::emojipick::Target::React(id));
                cx.notify();
            }
            None => cx.notify(),
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
                    move |e: &MouseUpEvent, phase, window, cx| {
                        if phase.bubble() && e.button == MouseButton::Left {
                            let _ = weak.update(cx, |this, cx| this.pointer_released(e.position, window, cx));
                        }
                    }
                });
                window.on_mouse_event(move |e: &LongPressEvent, phase, window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    let _ = weak.update(cx, |this, cx| match e.phase {
                        TouchPhase::Moved => this.pointer_moved(e.position, cx),
                        TouchPhase::Ended => this.pointer_released(e.position, window, cx),
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
                            let _ = weak.update(cx, |this, cx| this.press_start(id.clone(), e.position, cx));
                        }
                    }
                });
                // A finger landing here: the bubble tints after `ROW_PRESS_DELAY` unless the touch turns
                // into a scroll first. A touch reaches the app as a click only when it lifts.
                let visible = window.content_mask().bounds;
                window.on_mouse_event({
                    let (weak, id) = (weak.clone(), id.clone());
                    move |e: &TouchDragEvent, phase, _, cx| {
                        let at = e.start_position;
                        if phase.bubble() && e.phase == TouchPhase::Started && bounds.contains(&at) && visible.contains(&at) {
                            let _ = weak.update(cx, |this, cx| this.bubble_pressed(id.clone(), cx));
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
                    app.update(cx, |this, cx| {
                        this.bubble_press = None;
                        this.open_menu(id.clone(), MenuKind::Touch, e.start_position, Some(rest), true, cx);
                    });
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

    /// A tap on a message shows or hides its time and status line; a double tap likes it.
    pub(super) fn bubble_click(&mut self, id: &str, event: &ClickEvent, cx: &mut Context<Self>) {
        if event.click_count() == 2 && id.starts_with('$') {
            self.double_tap(id, cx);
        }
        // A tap shows or hides a message's status line, for the user's who has read it; the double
        // tap's second click undoes its first.
        let swiped = self.swipe_back.as_ref().is_some_and(|(on, _, at)| on == id && at.elapsed() < Duration::from_millis(600));
        if event.click_count() <= 2 && !swiped && !self.menu_open() && !self.fwd.selecting() {
            self.tapped = if self.tapped.as_deref() == Some(id) { None } else { Some(id.to_owned()) };
            cx.notify();
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
            .when(sent && self.caps_for(&m.id).reaction >= 1, |el| {
                el.child(button("react", IconName::Heart).on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    this.open_menu(react.clone(), MenuKind::Picker, e.position(), None, false, cx)
                })))
            })
            .when(sent && self.caps_for(&m.id).reply >= 1, |el| {
                el.child(button("reply", IconName::Reply).on_click(cx.listener(move |this, _, window, cx| {
                    this.start_compose(Compose::Reply(reply.clone()), window, cx)
                })))
            })
            .child(button("more", IconName::Ellipsis).on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                this.open_menu(more.clone(), MenuKind::List, e.position(), None, false, cx)
            })))
    }

    pub(super) fn act(&mut self, id: &str, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::Reply => self.start_compose(Compose::Reply(id.to_owned()), window, cx),
            Act::Thread => self.switch_thread(Some(id.to_owned()), cx),
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
            Act::Delete if id.starts_with('$') => self.ask_delete(vec![id.to_owned()], cx),
            Act::Delete => {
                if let Some(room) = self.open.clone() {
                    self.send(Cmd::Delete { room, target: id.to_owned() }, cx);
                }
            }
            Act::EndPoll => {
                if let Some(room) = self.open.clone() {
                    self.send(Cmd::EndPoll { room, target: id.to_owned() }, cx);
                }
            }
            Act::Link(url) => crate::open_link(cx, &url),
            Act::CopyLink(url) => crate::copy(cx, url),
            Act::Copy(body) => crate::copy(cx, body),
            Act::SelectText(body) => self.select_text(body, window, cx),
            Act::Save => self.export_message(id, crate::backend::Export::Save, cx),
            Act::Forward => self.forward_start(vec![id.to_owned()], window, cx),
            Act::Select => self.select_start(id, cx),
            Act::Pin | Act::Unpin => {
                if let Some(room) = self.open.clone() {
                    self.send(Cmd::Pin { room, target: id.to_owned(), pin: act == Act::Pin }, cx);
                }
            }
            Act::Info => {
                let at = self.menu.as_ref().filter(|m| m.kind != MenuKind::Touch).map(|m| m.at);
                self.open_people(super::people::Shown::Details(id.to_owned()), at, cx);
            }
            Act::Reactions => {
                let at = self.menu.as_ref().filter(|m| m.kind != MenuKind::Touch).map(|m| m.at);
                self.open_people(super::people::Shown::Reactions(id.to_owned(), None), at, cx);
            }
        }
        self.close_menu(cx);
    }

    /// Opens the chat's sheet with `body` in a read-only field, where a press and hold (a drag with
    /// a pointer) selects any part of it and the field's own menu copies it.
    fn select_text(&mut self, body: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let field = cx.new(|cx| gpui_kit::component::input::TextareaState::new(window, cx).auto_grow(1, SELECT_TEXT_ROWS));
        field.update(cx, |field, cx| field.set_value(body, window, cx));
        // The field cannot be typed in, yet focusing it shows the keyboard (gpui-mobile's FocusGained,
        // in the draw after the focus): hide it again after that draw.
        let sub = cx.subscribe_in(&field, window, |_, _, ev: &InputEvent, window, _| {
            if matches!(ev, InputEvent::Focus) {
                window.on_next_frame(|window, _| window.on_next_frame(|_, _| crate::hide_keyboard()));
            }
        });
        // From the bottom with a mouse too: the list at the pointer has rows of one fixed height.
        self.open_chat_sheet(room, None, false, cx);
        self.select_text = Some((field, sub));
    }

    /// The sheet's one row: the field, and on a phone how to select in it.
    pub(super) fn select_text_rows(&self, cx: &mut Context<Self>) -> Option<Vec<Stateful<Div>>> {
        let (field, _) = self.select_text.as_ref()?;
        let p = Palette::of(cx);
        let row = v_flex()
            .id("select-text")
            .px(px(GUTTER))
            .pb(px(GAP))
            .gap(px(8.))
            .child(gpui_kit::component::input::Textarea::new(field).readonly(true).bordered(false).text_size(fs(16.)))
            .when(gpui_kit::is_mobile(), |el| el.child(div().text_size(fs(13.)).text_color(p.muted).child(t!("Press and hold a word to select it."))));
        Some(vec![row])
    }

    /// The actions as rows; `picked` is lit, under a finger that will pick it on letting go.
    fn act_rows(&self, id: &str, acts: Vec<Act>, h: f32, picked: Option<usize>, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let p = Palette::of(cx);
        acts.into_iter()
            .enumerate()
            .map(|(n, act)| {
                let (name, icon, label) = act.look();
                let ink = if act == Act::Delete { p.negative } else { p.text };
                let id = id.to_owned();
                h_flex()
                    .id(name)
                    .role(Role::MenuItem)
                    .aria_label(label)
                    // The first act takes the focus when the keyboard opened the menu.
                    .when(n == 0 && !gpui_kit::is_mobile(), |el| el.track_focus(&self.keynav.menu))
                    .when(!gpui_kit::is_mobile(), |el| super::keynav::reachable(el, cx))
                    .h(px(h))
                    .px(px(GUTTER))
                    .gap(px(GUTTER))
                    .text_size(fs(16.))
                    .text_color(ink)
                    .cursor_pointer()
                    .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.text.opacity(0.1))))
                    .active(|el| el.bg(p.text.opacity(0.1)))
                    .when(picked == Some(n), |el| el.bg(p.text.opacity(0.1)))
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
        if menu.animating(now, cx.reduce_motion()) {
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
        let view = frame::viewport(window);
        let menu = self.menu.as_ref()?;
        let m = self.msgs.iter().find(|m| m.id == menu.id)?;
        let id = m.id.clone();
        let caps = self.caps_for(&id);
        let keys = bar_keys(m, caps, &quick(&self.settings));
        let more = more_offered(&keys, caps);
        let reacts = id.starts_with('$') && caps.reaction >= 1;
        let acts = self.with_pin(taken(acts(m, MenuKind::Touch, false), m, caps), m);
        let mine: Vec<bool> = keys.iter().map(|k| m.reactions.iter().any(|r| r.own && same_emoji(&r.key, k))).collect();
        let b = Bounds { origin: frame::local(window, menu.bubble.origin), size: menu.bubble.size };
        let bubble = Bounds { origin: point(f32::from(b.origin.x), f32::from(b.origin.y)), size: size(f32::from(b.size.width), f32::from(b.size.height)) };
        let placed = place(
            size(f32::from(view.width), f32::from(view.height)),
            crate::insets(),
            bubble,
            f32::from(frame::local(window, menu.at).y),
            m.own,
            reacts.then_some(keys.len() + more as usize),
            ROW_H * acts.len() as f32 + 16.,
        );
        let placed = Placement { rows: acts.len(), ..placed };
        let menu = self.menu.as_mut()?;
        menu.placed = Some(placed);
        let menu = self.menu.as_ref()?;
        let open = menu.closing.is_none();
        let shade = if still { 1. } else { menu.reveal(now, 0., 200., decelerate) };
        let v_list = menu.reveal(now, 150., 200., decelerate);
        let rise = |n: f32| if still { 0. } else { EMOJI_RISE * (1. - menu.reveal(now, 100. + 10. * n, 200., decelerate)) };
        let bar = placed.bar.map(|(x, y, _)| {
            let more = more.then(|| self.more_reactions(&id, 1., cx).opacity(menu.reveal(now, 100. + 10. * keys.len() as f32, 200., decelerate)));
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
                    // The ring gives way to the grown emoji, which would cut through it.
                    .when(mine[n], |el| el.border_2().border_color(p.text.opacity(menu.reveal(now, 0., 400., decelerate) * (1. - grown))))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key, cx)))
                    .child(
                        div()
                            .relative()
                            .top(px(rise(n as f32) - EMOJI_RISE * grown))
                            .opacity(menu.reveal(now, 100. + 10. * n as f32, 200., decelerate))
                            .text_size(px((EMOJI + (EMOJI_GROWN - EMOJI) * grown).round()))
                            .line_height(px(EMOJI_GROWN))
                            .child(crate::emoji::styled(clip_key(&label), Vec::new())),
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
                        .children(slots)
                        .children(more),
                ),
            )
            .with_priority(OVER_LIFTED)
        });
        let rows = self.act_rows(&id, acts, ROW_H, menu.pick, cx);
        let (lx, ly) = placed.list;
        // Signal's list waits 150 ms for the message to settle, then fades in over 200 ms.
        let list = deferred(
            div().absolute().left(px(lx)).top(px(if still { ly } else { ly - LIST_RISE * (1. - v_list) })).child(
                v_flex()
                    .id("message-actions")
                    .w(fs(LIST_W))
                    .py(px(LIST_PAD))
                    .rounded(px(RADIUS))
                    .bg(p.lifted)
                    .opacity(v_list)
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
        let view = frame::viewport(window);
        let menu = self.menu.as_ref()?;
        let m = self.msgs.iter().find(|m| m.id == menu.id)?;
        let (id, keys) = (m.id.clone(), bar_keys(m, self.caps_for(&m.id), &quick(&self.settings)));
        let more = more_offered(&keys, self.caps_for(&id));
        let w = (keys.len() + more as usize) as f32 * PICK + 2. * PICK_PAD_X;
        let h = PICK + 2. * PICK_PAD_Y;
        let (top, bottom) = crate::insets();
        let at = frame::local(window, menu.at);
        let at = (f32::from(at.x), f32::from(at.y));
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
            // The cell, its disc and its hover stay in the bar; only the emoji rises and grows, so the
            // disc never leaves the pill and the pointer never loses the cell it lifted.
            div()
                .id(("picker-reaction", n))
                .relative()
                .top(px(24. * (1. - shown)))
                .opacity(shown)
                .size(px(PICK))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .when(mine, |el| el.border_2().border_color(p.text.opacity(1. - grow)))
                .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                    let next = if *over { Some(n) } else { this.picker_hover.filter(|&h| h != n) };
                    if next != this.picker_hover {
                        this.picker_hover = next;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key, cx)))
                .child(
                    div()
                        .relative()
                        .top(px(-PICK_RISE * grow))
                        .text_size(px(PICK_EMOJI + (PICK_GROWN - PICK_EMOJI) * grow))
                        .line_height(px(PICK_GROWN))
                        .child(crate::emoji::styled(clip_key(&label), Vec::new())),
                )
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
                .children(buttons)
                .when(more, |el| el.child(self.more_reactions(&id, PICK / TAP, cx))),
        );
        Some(self.barrier(picker.into_any_element(), cx))
    }

    /// Signal Desktop's message menu, at the pointer.
    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let view = frame::viewport(window);
        let narrow = view.width < px(NARROW);
        let menu = self.menu.as_ref()?;
        let m = self.msgs.iter().find(|m| m.id == menu.id)?;
        let id = m.id.clone();
        let acts = self.with_pin(taken(acts(m, MenuKind::List, narrow), m, self.caps_for(&m.id)), m);
        let h = POINTER_ROW_H * acts.len() as f32 + 12.;
        let (top, bottom) = crate::insets();
        let at = frame::local(window, menu.at);
        let (ax, ay) = (f32::from(at.x), f32::from(at.y));
        let (vw, vh) = (f32::from(view.width), f32::from(view.height));
        let w = f32::from(fs(POINTER_LIST_W));
        let x = if ax + w <= vw - GUTTER { ax } else { ax - w };
        let y = if ay + h <= vh - bottom - GUTTER { ay } else { ay - h };
        let x = x.clamp(GUTTER, (vw - w - GUTTER).max(GUTTER));
        let y = y.clamp(top + GUTTER, (vh - bottom - h - GUTTER).max(top + GUTTER));
        let rows = self.act_rows(&id, acts, POINTER_ROW_H, None, cx);
        let list = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .child(v_flex().id("message-menu").w(fs(POINTER_LIST_W)).py(px(6.)).rounded(px(14.)).bg(p.lifted).occlude().children(rows))
            .with_animation(SharedString::from(format!("menu-list-{id}")), Animation::new(motion::ENTER).with_easing(motion::ease_out), move |el, t| {
                el.top(px(y + 4. * (1. - t))).opacity(t)
            });
        Some(self.barrier(list.into_any_element(), cx))
    }

    /// The pointer's lists close on a click or a right click anywhere else; nothing is dimmed.
    /// The + at the end of the bar: every other emoji, in the picker's sheet. `scale` sizes it
    /// against a bar slot.
    fn more_reactions(&self, id: &str, scale: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let id = id.to_owned();
        div()
            .id("bar-more")
            .size(px(TAP * scale))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .text_color(p.muted)
            .active(|el| el.bg(p.raised))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                this.emoji_pick.at = e.position();
                this.close_menu(cx);
                this.open_emoji(super::emojipick::Target::React(id.clone()), window, cx);
            }))
            .child(Icon::new(IconName::Plus).size_5())
    }

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
    use super::{Act, BAR_H, Delivery, Features, Kind, LIST_PAD, MenuKind, Msg, OVERLAY_GAP, Placement, QUICK_REACTIONS, ROW_H, TAP, acts, bar_keys, clip_key, ease_out_expo, more_offered, place, quick, row_at, same_emoji, set_quick, slot_at, taken};
    use gpui_kit::{Bounds, point, size};

    #[test]
    fn every_menu_lists_the_acts_in_one_order() {
        let mut m = Msg::bare("$1".to_owned(), Kind::Message, "@me:x".try_into().unwrap());
        (m.own, m.body) = (true, "see https://a.example".to_owned());
        let touch = acts(&m, MenuKind::Touch, false);
        assert_eq!(touch[..2], [Act::Reply, Act::Edit]);
        let pointer: Vec<_> = touch[1..].iter().filter(|a| !matches!(a, Act::SelectText(_))).cloned().collect();
        assert_eq!(acts(&m, MenuKind::List, false), pointer);
        assert_eq!(acts(&m, MenuKind::List, true)[..3], [Act::Reply, Act::React, Act::Edit]);
        assert_eq!(touch[touch.len() - 3..], [Act::Select, Act::Info, Act::Delete]);
        let mut liked = m.clone();
        liked.reactions.push(crate::backend::Reaction { key: "👍".to_owned(), count: 1, own: false, by: Vec::new() });
        let with = acts(&liked, MenuKind::Touch, false);
        assert_eq!(with[with.len() - 4..], [Act::Select, Act::Reactions, Act::Info, Act::Delete]);
        let mut theirs = m.clone();
        theirs.own = false;
        assert_eq!(acts(&theirs, MenuKind::Touch, false).last(), Some(&Act::Delete), "anyone's can be hidden here");

        let mut failed = Msg::bare("~1".to_owned(), Kind::Message, "@me:x".try_into().unwrap());
        (failed.own, failed.body, failed.delivery) = (true, "hi".to_owned(), Delivery::Failed(String::new()));
        let want = [Act::Retry, Act::Copy("hi".to_owned()), Act::SelectText("hi".to_owned()), Act::Delete];
        assert_eq!(acts(&failed, MenuKind::Touch, false), want);
        assert_eq!(acts(&failed, MenuKind::List, true), [Act::Retry, Act::Copy("hi".to_owned()), Act::Delete]);
    }

    #[test]
    fn the_chat_features_drop_what_the_network_does_not_take() {
        let mut m = Msg::bare("$1".to_owned(), Kind::Message, "@me:x".try_into().unwrap());
        (m.own, m.body, m.ts) = (true, "hi".to_owned(), 1);
        let all = acts(&m, MenuKind::List, true);
        assert_eq!(taken(all.clone(), &m, &Features::ALL), all);
        // An old message on WhatsApp: no edit past 15 minutes, no deletion past two days.
        let wa = Features { edit_max_age: Some(900), delete_max_age: Some(172_800), ..Features::ALL };
        let left = taken(all.clone(), &m, &wa);
        assert!(left.contains(&Act::Reply) && left.contains(&Act::React) && left.contains(&Act::Delete));
        assert!(!left.contains(&Act::Edit));
        let none = Features { reply: 0, reaction: -2, ..Features::ALL };
        let left = taken(all, &m, &none);
        assert!(!left.contains(&Act::Reply) && !left.contains(&Act::React) && left.contains(&Act::Edit));
    }

    #[test]
    fn the_bar_offers_only_allowed_reactions() {
        let m = Msg::bare("$1".to_owned(), Kind::Message, "@me:x".try_into().unwrap());
        let six = QUICK_REACTIONS.map(str::to_owned);
        assert_eq!(bar_keys(&m, &Features::ALL, &six).len(), 6);
        assert!(more_offered(&bar_keys(&m, &Features::ALL, &six), &Features::ALL));
        let two = Features { allowed_reactions: Some(vec!["\u{1F44D}".to_owned(), "\u{1F525}".to_owned()]), ..Features::ALL };
        assert_eq!(bar_keys(&m, &two, &six), ["\u{1F44D}"]);
        assert!(more_offered(&bar_keys(&m, &two, &six), &two), "the fire is only behind the +");
        let fire = Features { allowed_reactions: Some(vec!["\u{1F525}".to_owned()]), ..Features::ALL };
        assert_eq!(bar_keys(&m, &fire, &six), ["\u{1F525}"]);
        assert!(!more_offered(&bar_keys(&m, &fire, &six), &fire));
    }

    #[test]
    fn a_chosen_reaction_takes_its_slot_and_trades_places_with_a_twin() {
        let mut s = crate::Settings::load(std::path::Path::new("/nonexistent"));
        set_quick(&mut s, 1, "\u{1F525}");
        assert_eq!(quick(&s)[1], "\u{1F525}");
        set_quick(&mut s, 0, "\u{1F525}");
        assert_eq!(quick(&s)[..2], ["\u{1F525}", "\u{2764}\u{FE0F}"]);
        set_quick(&mut s, 1, "\u{1F525}");
        set_quick(&mut s, 1, "\u{1F44D}");
        assert!(s.reactions.is_empty(), "back to Signal's six, nothing kept");
        set_quick(&mut s, 6, "\u{1F525}");
        assert_eq!(s.double_tap.as_deref(), Some("\u{1F525}"));
        set_quick(&mut s, 6, "\u{2764}");
        assert_eq!(s.double_tap, None);
        set_quick(&mut s, 9, "\u{1F525}");
        assert!(s.reactions.is_empty());
    }

    #[test]
    fn long_reaction_keys_are_cut_and_emoji_kept_whole() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        assert_eq!(clip_key(family), family);
        assert_eq!(clip_key("\u{2764}"), "\u{2764}\u{FE0F}");
        let long = "a".repeat(200);
        assert_eq!(clip_key(&long).chars().count(), 16);
        assert!(clip_key(&long).ends_with('\u{2026}'));
    }

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
    fn a_finger_slid_onto_the_list_picks_the_row_under_it() {
        let p = Placement { rows: 4, ..at(400., 60., false) };
        let (x, y) = p.list;
        assert_eq!(row_at(&p, point(x + 20., y + LIST_PAD + ROW_H * 1.5)), Some(1));
        assert_eq!(row_at(&p, point(x + 20., y + LIST_PAD + 1.)), Some(0));
        assert_eq!(row_at(&p, point(x + 20., y + LIST_PAD + ROW_H * 4. + 1.)), None, "under the last row");
        assert_eq!(row_at(&p, point(x - 1., y + LIST_PAD + ROW_H)), None, "beside the list");
        assert_eq!(row_at(&p, point(x + 20., y)), None, "above the first row");
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
