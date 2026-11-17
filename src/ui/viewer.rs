//! The full-screen picture: pinch, double-tap or ctrl+wheel to zoom, drag to pan, and Save and
//! Share. GPUI cannot scale an element, so a zoomed picture is laid out larger than the screen
//! inside an `overflow: hidden` box and moved by an offset that is kept inside the picture's edges.
//!
//! A fitted picture follows a finger: sideways to the chat's previous or next picture, up or down
//! to close. A tap shows or hides the bars with the sender and the actions.
//!
//! A chat's or a person's profile picture opens here too, on its own: named after them, without
//! paging or actions.

use std::collections::VecDeque;
use std::io::Cursor;

use super::*;
use super::motion::Glide;
use crate::backend::Export;
use crate::i18n::t;

/// A double-tap zooms in this far.
const DOUBLE_TAP_SCALE: f32 = 2.5;
const MAX_SCALE: f32 = 6.;
/// Two taps this close in time and place are a double-tap; a single tap waits this long to close.
const DOUBLE_TAP: Duration = Duration::from_millis(300);
const DOUBLE_TAP_SLOP: f32 = 48.;
/// A press that moved less than this (px) is still a tap.
const DRAG_SLOP: f32 = 4.;
const ZOOM_MS: u64 = 220;
const NOTE_MS: u64 = 3000;
/// One wheel notch with ctrl held zooms by about this much per pixel of delta.
const WHEEL_ZOOM: f32 = 0.005;
/// Black between two pictures side by side while one slides in.
const PAGE_GAP: f32 = 24.;
/// A release faster than this (px/s) toward a neighbour turns to it, however short the pull.
const TURN_SPEED: f32 = 500.;
/// Past this share of the screen's height a pull up or down closes the picture.
const DISMISS_SHARE: f32 = 0.15;
const DISMISS_SPEED: f32 = 800.;
/// Past the last picture a pull moves the picture this share of the finger's travel.
const EDGE_GIVE: f32 = 0.3;
/// The bars' height: a row of buttons with room for a label under each icon.
const ACTIONS_H: f32 = 64.;
/// Past the edge of a zoomed picture a finger moves it `EDGE_GIVE` of its travel, at most this share of the screen.
const OVERPAN: f32 = 0.15;
/// A picture pulled up or down shrinks by this share at most, reached at `SHRINK_AT` of the screen's height.
const SHRINK: f32 = 0.3;
const SHRINK_AT: f32 = 0.6;
/// A pinch below the fitted size stops shrinking here; let go below `PINCH_CLOSE`, the picture closes.
const PINCH_MIN: f32 = 0.6;
const PINCH_CLOSE: f32 = 0.8;
/// The bars follow the picture in, once it has mostly landed.
const BARS_DELAY: Duration = Duration::from_millis(120);
/// A notice rises this far as it fades in.
const NOTE_RISE: f32 = 8.;

#[derive(Clone, Copy, PartialEq, Debug)]
struct View {
    scale: f32,
    /// Where the picture's centre is from the screen's centre.
    off: (f32, f32),
}

const FIT: View = View { scale: 1., off: (0., 0.) };

/// The picture as `contain` draws it at scale 1.
fn fitted(screen: (f32, f32), dims: Option<(f32, f32)>) -> (f32, f32) {
    match dims {
        Some((w, h)) if w > 0. && h > 0. => {
            let k = (screen.0 / w).min(screen.1 / h);
            (w * k, h * k)
        }
        _ => screen,
    }
}

/// How far the centre may move from the screen's centre: until the edge of the picture meets the
/// edge of the screen, and not at all along a side that is still smaller than the screen.
fn reach(scale: f32, screen: (f32, f32), fit: (f32, f32)) -> (f32, f32) {
    (((fit.0 * scale - screen.0) / 2.).max(0.), ((fit.1 * scale - screen.1) / 2.).max(0.))
}

fn clamped(view: View, screen: (f32, f32), fit: (f32, f32)) -> View {
    let scale = view.scale.clamp(1., MAX_SCALE);
    let (rx, ry) = reach(scale, screen, fit);
    View { scale, off: (view.off.0.clamp(-rx, rx), view.off.1.clamp(-ry, ry)) }
}

/// Zooms to `scale` keeping the picture point under `at` (from the screen's centre) where it is.
fn zoomed_about(view: View, at: (f32, f32), scale: f32, screen: (f32, f32), fit: (f32, f32)) -> View {
    let scale = scale.clamp(1., MAX_SCALE);
    let k = scale / view.scale;
    clamped(View { scale, off: (at.0 - (at.0 - view.off.0) * k, at.1 - (at.1 - view.off.1) * k) }, screen, fit)
}

/// A picture's place on screen: its centre and size.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn of(b: Bounds<Pixels>) -> Self {
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        Self { x: f32::from(b.origin.x) + w / 2., y: f32::from(b.origin.y) + h / 2., w, h }
    }

    fn lerp(self, to: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self { x: mix(self.x, to.x), y: mix(self.y, to.y), w: mix(self.w, to.w), h: mix(self.h, to.h) }
    }
}

/// How large a fitted picture pulled `dy` up or down is drawn.
fn shrink(dy: f32, height: f32) -> f32 {
    1. - SHRINK * (dy.abs() / (SHRINK_AT * height)).min(1.)
}

/// Where the picture is drawn: `view`, moved by `drift` and shrunk to `s` about `grab`, the point
/// the finger took (offsets from the screen's centre), so that point stays under the finger.
fn placed(view: View, screen: (f32, f32), fit: (f32, f32), drift: (f32, f32), grab: (f32, f32), s: f32) -> Rect {
    Rect {
        x: screen.0 / 2. + view.off.0 + drift.0 + grab.0 * (1. - s),
        y: screen.1 / 2. + view.off.1 + drift.1 + grab.1 * (1. - s),
        w: fit.0 * view.scale * s,
        h: fit.1 * view.scale * s,
    }
}

/// A zoomed picture's offset along one axis for a finger's `raw` one: 1:1 up to the edge, past it
/// `EDGE_GIVE` of the travel, at most `OVERPAN` of the screen.
fn rubber(raw: f32, reach: f32, screen: f32) -> f32 {
    let over = raw.abs() - reach;
    if over <= 0. { raw } else { raw.signum() * (reach + (over * EDGE_GIVE).min(screen * OVERPAN)) }
}

/// The scale drawn for a pinch's `raw` one: past the largest and below the fitted size it meets
/// resistance, and it never shrinks below `PINCH_MIN`.
fn resist(raw: f32) -> f32 {
    if raw > MAX_SCALE {
        MAX_SCALE * (raw / MAX_SCALE).powf(0.35)
    } else if raw < 1. {
        raw.max(0.).sqrt().max(PINCH_MIN)
    } else {
        raw
    }
}

/// How much of the black backdrop is left behind a picture pinched below its fitted size.
fn pinch_shade(scale: f32) -> f32 {
    ((scale - PINCH_MIN) / (1. - PINCH_MIN)).clamp(0., 1.)
}

/// How far a thrown picture has landed, 0 where it was let go and 1 in its bubble, by the side
/// that travels furthest.
fn landed(now: Rect, from: Rect, to: Rect) -> f32 {
    let (dy, dw) = ((from.y - to.y).abs(), (from.w - to.w).abs());
    let (at, far) = if dy >= dw { ((now.y - to.y).abs(), dy) } else { ((now.w - to.w).abs(), dw) };
    if far < 0.5 { 1. } else { (1. - at / far).clamp(0., 1.) }
}

fn glide((response, damping): (Duration, f32), pos: f32) -> Glide {
    Glide::new(response, damping, 0.5, pos)
}

/// The picture's offset from where it rests: a finger's pull on a fitted picture and the spring
/// after it, or how far a zoomed one is past its edge.
#[derive(Clone, Copy)]
struct Drift {
    x: Glide,
    y: Glide,
}

impl Default for Drift {
    fn default() -> Self {
        Self { x: glide(motion::SETTLE, 0.), y: glide(motion::SETTLE, 0.) }
    }
}

/// The bubble the picture on screen belongs to, as last painted: where it flies back to.
struct Home {
    id: String,
    bounds: Bounds<Pixels>,
    radius: f32,
    /// The frame that painted it; bounds from an older one are of a bubble no longer on screen.
    frame: u64,
}

/// How the picture leaves.
#[derive(Default)]
enum Exit {
    #[default]
    Fade,
    /// A key: gone in the next frame.
    Instant,
    /// Back into its bubble.
    Hero,
    /// Let go by a finger, at its speed.
    Throw(Box<Throw>),
}

/// A picture let go of on its way out: into its bubble when that is on screen, else off the
/// screen while it fades.
struct Throw {
    /// Centre x, y, width, height.
    rect: [Glide; 4],
    from: Rect,
    /// How much backdrop there was when the finger let go.
    shade: f32,
    at: Instant,
    home: bool,
}

/// A finger on a zoomed picture: where it would have it without the edges, and its recent path.
struct Pan {
    raw: (f32, f32),
    xs: VecDeque<(Instant, f32)>,
    ys: VecDeque<(Instant, f32)>,
}

/// Two fingers: the scale they have pinched to since they landed, unlimited, and the view and
/// midpoint then.
struct Pinch {
    raw: f32,
    from: View,
    at: (f32, f32),
}

#[derive(Default)]
pub(super) struct Zoom {
    view: Option<View>,
    /// A zoom in motion: where it began and when.
    glide: Option<(View, Instant)>,
    pinching: bool,
    mouse: Option<Point<Pixels>>,
    dragged: f32,
    tap: Option<(Instant, Point<Pixels>)>,
    /// Screen and picture size of the last frame, which the handlers work with.
    screen: (f32, f32),
    /// Where the screen starts in the window, whose coordinates pointer positions are in.
    origin: Point<Pixels>,
    fit: (f32, f32),
    dims: Option<(u64, (f32, f32))>,
    working: Option<Export>,
    note: Option<(SharedString, u64)>,
    epoch: u64,
    /// A finger moving the fitted picture.
    pull: Option<Pull>,
    drift: Drift,
    /// Where the finger took a fitted picture it pulls, from the screen's centre.
    grip: (f32, f32),
    pan: Option<Pan>,
    /// When the last step of a zoomed picture's fling arrived, while the fling coasts on.
    coast: Option<Instant>,
    pinch: Option<Pinch>,
    /// Counts rendered frames, to tell a bubble painted in the last one.
    frame: u64,
    home: Option<Home>,
    /// It flew out of its bubble.
    hero: bool,
    exit: Exit,
    /// The message whose bubble the picture is drawn over, which leaves its thumbnail out.
    covers: Option<String>,
    /// The picture has been drawn in its bubble for the last time; the next frame lets it go.
    landed: bool,
    /// What was drawn last frame, where a finger lets go from.
    drawn: Rect,
    shade: f32,
    /// The last notice, which stays while it fades out.
    note_text: SharedString,
    /// The bars are hidden.
    bare: bool,
    /// The pictures beside the one on screen, loaded or on their way: at most one each side.
    near: Vec<(String, Option<Arc<Image>>)>,
    /// The oldest loaded picture older history was last asked for from.
    older_from: Option<String>,
    /// Counts openings, so springs and fades start afresh with each picture opened.
    serial: u64,
    /// The name over a profile picture, in place of a message's sender.
    title: Option<String>,
}

/// The viewer's id for a profile picture: a person's (`@user`) or a chat's (`!room`). Never an
/// event's (`$`) or a message on its way out's.
const PICTURE: &str = "picture ";

fn is_picture(id: &str) -> bool {
    id.starts_with(PICTURE)
}

/// A finger on a fitted picture. The axis is the first one it moved along; GPUI locks a touch pan
/// to it anyway.
struct Pull {
    across: Option<bool>,
    offset: f32,
    samples: VecDeque<(Instant, f32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Turn {
    Stay,
    Older,
    Newer,
}

/// Where a sideways pull lets go: `offset` is the drawn one, positive when the finger moved right,
/// which brings the older picture in from the left.
fn settle(offset: f32, speed: f32, width: f32, older: bool, newer: bool) -> Turn {
    let far = offset.abs() > width * 0.5;
    let flung = speed.abs() > TURN_SPEED && speed.signum() == offset.signum();
    if !(far || flung) || offset == 0. {
        Turn::Stay
    } else if offset > 0. && older {
        Turn::Older
    } else if offset < 0. && newer {
        Turn::Newer
    } else {
        Turn::Stay
    }
}

/// Whether a pull up or down lets go far or fast enough to close the picture.
fn dismisses(offset: f32, speed: f32, height: f32) -> bool {
    offset.abs() > height * DISMISS_SHARE || (speed.abs() > DISMISS_SPEED && speed.signum() == offset.signum())
}

/// How far the picture follows a sideways pull: 1:1 toward a neighbour, with resistance past the end.
fn give(offset: f32, older: bool, newer: bool) -> f32 {
    let open = if offset > 0. { older } else { newer };
    if open { offset } else { offset * EDGE_GIVE }
}

/// The pictures of a chat that can be browsed, oldest first: images and stickers not deleted.
fn pictures<'a>(msgs: impl IntoIterator<Item = (&'a str, Option<MediaKind>, bool)>) -> Vec<&'a str> {
    msgs.into_iter().filter(|(_, kind, gone)| !gone && matches!(kind, Some(MediaKind::Image | MediaKind::Sticker))).map(|(id, ..)| id).collect()
}

/// The pictures beside `id` in `set`: (older, newer). The only ones kept loaded besides it.
fn beside<'a>(set: &[&'a str], id: &str) -> (Option<&'a str>, Option<&'a str>) {
    let Some(ix) = set.iter().position(|s| *s == id) else { return (None, None) };
    (ix.checked_sub(1).map(|i| set[i]), set.get(ix + 1).copied())
}

impl Zoom {
    fn target(&self) -> View {
        self.view.unwrap_or(FIT)
    }

    /// What is drawn now: the target, or part of the way there.
    fn shown(&self, reduce_motion: bool) -> (View, bool) {
        let to = self.target();
        match self.glide {
            Some((from, at)) if !reduce_motion => {
                let t = (at.elapsed().as_secs_f32() / (ZOOM_MS as f32 / 1000.)).min(1.);
                let k = motion::ease_out(t);
                let mix = |a: f32, b: f32| a + (b - a) * k;
                (View { scale: mix(from.scale, to.scale), off: (mix(from.off.0, to.off.0), mix(from.off.1, to.off.1)) }, t < 1.)
            }
            _ => (to, false),
        }
    }

    /// A finger or wheel takes over from whatever was gliding.
    fn grab(&mut self, reduce_motion: bool) -> View {
        let now = self.shown(reduce_motion).0;
        self.glide = None;
        self.view = Some(now);
        now
    }

    fn set(&mut self, view: View) {
        self.view = Some(clamped(view, self.screen, self.fit));
    }

    /// A pointer position from the screen's centre.
    fn centred(&self, at: Point<Pixels>) -> (f32, f32) {
        let at = at - self.origin;
        (f32::from(at.x) - self.screen.0 / 2., f32::from(at.y) - self.screen.1 / 2.)
    }

    fn zoom_by(&mut self, at: Point<Pixels>, factor: f32, reduce_motion: bool) {
        let now = self.grab(reduce_motion);
        let at = self.centred(at);
        self.view = Some(zoomed_about(now, at, now.scale * factor, self.screen, self.fit));
    }

    fn pan(&mut self, dx: f32, dy: f32, reduce_motion: bool) {
        let now = self.grab(reduce_motion);
        self.set(View { off: (now.off.0 + dx, now.off.1 + dy), ..now });
    }

    /// A tap at `at`. True when it completes a double-tap, which zooms in about the tap or back out.
    fn tapped(&mut self, at: Point<Pixels>, reduce_motion: bool) -> bool {
        let now = Instant::now();
        let near = |a: Point<Pixels>| (f32::from(a.x - at.x)).hypot(f32::from(a.y - at.y)) <= DOUBLE_TAP_SLOP;
        let double = self.tap.is_some_and(|(then, there)| now.duration_since(then) <= DOUBLE_TAP && near(there));
        if !double {
            self.tap = Some((now, at));
            return false;
        }
        self.tap = None;
        let from = self.grab(reduce_motion);
        let to = if from.scale > 1.05 {
            FIT
        } else {
            zoomed_about(from, self.centred(at), DOUBLE_TAP_SCALE, self.screen, self.fit)
        };
        self.view = Some(to);
        self.glide = Some((from, now));
        true
    }

    fn zoomed_in(&self) -> bool {
        self.target().scale > 1.01
    }

    /// `id`'s bubble, when the frame numbered `frame` painted it.
    fn home_at(&self, id: &str, frame: u64) -> Option<&Home> {
        self.home.as_ref().filter(|h| h.id == id && h.frame == frame)
    }

    /// `id`'s bubble, when the last frame painted it.
    fn home_of(&self, id: &str) -> Option<&Home> {
        self.home_at(id, self.frame)
    }

    /// A bubble's picture was painted at `bounds`; kept when it is the one followed.
    pub(super) fn seen(&mut self, id: &str, bounds: Bounds<Pixels>, radius: f32) {
        if let Some(home) = self.home.as_mut().filter(|h| h.id == id) {
            (home.bounds, home.radius, home.frame) = (bounds, radius, self.frame);
        }
    }

    /// A press on a bubble's picture, which a tap may open from.
    pub(super) fn pressed(&mut self, id: &str, bounds: Bounds<Pixels>, radius: f32) {
        self.home = Some(Home { id: id.to_owned(), bounds, radius, frame: self.frame });
    }

    /// Whether the viewer draws over `id`'s bubble, which then leaves its own picture out.
    pub(super) fn covers(&self, id: &str) -> bool {
        self.covers.as_deref() == Some(id)
    }
}

fn dimensions(image: &Image) -> Option<(f32, f32)> {
    let (w, h) = image::ImageReader::new(Cursor::new(&image.bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some((w as f32, h as f32))
}

impl BeeperApp {
    /// Opens a profile picture full-screen: `user`'s as this chat shows them, or with `None` the
    /// chat's. The small picture stands in until the large one arrives.
    pub(super) fn open_picture(&mut self, room: OwnedRoomId, user: Option<OwnedUserId>, name: String, cx: &mut Context<Self>) {
        let id = format!("{PICTURE}{}", user.as_ref().map_or(room.as_str(), |u| u.as_str()));
        if self.send(Cmd::ViewPicture { id: id.clone(), room, user }, cx) {
            self.viewer = Some((id, None));
            self.open_zoom(cx);
            self.zoom.title = Some(name);
        }
    }

    /// The small profile picture behind a picture id, already loaded for its row.
    fn stand_in(&self, id: &str) -> Option<Arc<Image>> {
        let key = id.strip_prefix(PICTURE)?;
        match OwnedUserId::try_from(key) {
            Ok(user) => self.faces.get(&user).cloned().flatten(),
            Err(_) => self.avatars.get(&OwnedRoomId::try_from(key).ok()?).cloned().flatten(),
        }
    }

    pub(super) fn open_zoom(&mut self, cx: &mut Context<Self>) {
        self.drop_near(cx);
        let (frame, home) = (self.zoom.frame, self.zoom.home.take());
        self.zoom = Zoom { serial: self.zoom.serial + 1, frame, home, ..Zoom::default() };
        // Out of its bubble only when that was on screen in the last frame, with a picture to fly.
        self.zoom.hero = !cx.reduce_motion() && self.viewer.as_ref().is_some_and(|(id, _)| self.thumbs.contains_key(id) && self.zoom.home_of(id).is_some());
        self.fill_near(cx);
    }

    /// The full picture for the one on screen or a neighbour; an answer for neither is dropped.
    pub(super) fn viewed(&mut self, id: String, bytes: Vec<u8>) {
        let Some(format) = image_format(&bytes) else { return };
        let slot = match &mut self.viewer {
            Some((open, slot)) if *open == id => slot,
            _ => match self.zoom.near.iter_mut().find(|(near, _)| *near == id) {
                Some((_, slot)) => slot,
                None => return,
            },
        };
        if slot.is_none() {
            *slot = Some(Arc::new(Image::from_bytes(format, bytes)));
        }
    }

    pub(super) fn drop_near(&mut self, cx: &mut Context<Self>) {
        for (_, image) in self.zoom.near.drain(..) {
            if let Some(image) = image {
                self.released.push(image, cx);
            }
        }
    }

    /// The pictures before and after the one on screen, among those of the open chat.
    fn neighbours(&self) -> (Option<String>, Option<String>) {
        let Some((id, _)) = &self.viewer else { return (None, None) };
        let set = pictures(self.msgs.iter().filter(|m| !self.is_hidden(&m.id)).map(|m| (m.id.as_str(), m.media.as_ref().map(|media| media.kind), m.deleted || m.media.as_ref().is_some_and(|media| media.once))));
        let (older, newer) = beside(&set, id);
        (older.map(str::to_owned), newer.map(str::to_owned))
    }

    /// Keeps the neighbours' full pictures, frees any other, and asks for the missing ones.
    fn fill_near(&mut self, cx: &mut Context<Self>) {
        let (older, newer) = self.neighbours();
        let wanted = [older, newer];
        let mut kept = Vec::with_capacity(2);
        for (id, image) in std::mem::take(&mut self.zoom.near) {
            if wanted.iter().flatten().any(|w| *w == id) {
                kept.push((id, image));
            } else if let Some(image) = image {
                self.released.push(image, cx);
            }
        }
        for id in wanted.into_iter().flatten() {
            let Some(ix) = self.msgs.iter().position(|m| m.id == id) else { continue };
            if kept.iter().any(|(k, _)| *k == id) {
                continue;
            }
            if !self.thumbs.contains_key(&id) {
                self.want_thumb(ix);
            }
            // Ahead of the finger, so only with half the command channel left to user actions.
            let Some(media) = self.msgs[ix].media.clone() else { continue };
            if self.cmd.capacity() >= self.cmd.max_capacity() / 2 && self.cmd.try_send(Cmd::View(id.clone(), media)).is_ok() {
                kept.push((id, None));
            }
        }
        self.zoom.near = kept;
    }

    /// Shows the next newer or older picture of the chat. At the oldest loaded one, asks once for
    /// older history, whose pictures then join the row.
    fn turn(&mut self, newer: bool, cx: &mut Context<Self>) -> bool {
        let (before, after) = self.neighbours();
        let Some(to) = (if newer { after } else { before }) else {
            self.reached_oldest(cx);
            return false;
        };
        let Some(was) = self.viewer.take() else { return false };
        let had = self.zoom.near.iter().position(|(id, _)| *id == to).map(|ix| self.zoom.near.remove(ix).1);
        self.zoom.near.push(was);
        // Its bubble is followed from the next frame that paints it.
        self.zoom.home = Some(Home { id: to.clone(), bounds: Bounds::default(), radius: 0., frame: u64::MAX });
        self.viewer = Some((to.clone(), had.clone().flatten()));
        if had.is_none()
            && let Some(media) = self.msgs.iter().find(|m| m.id == to).and_then(|m| m.media.clone())
        {
            self.send(Cmd::View(to, media), cx);
        }
        (self.zoom.view, self.zoom.glide, self.zoom.dims, self.zoom.tap) = (None, None, None, None);
        self.fill_near(cx);
        self.reached_oldest(cx);
        cx.notify();
        true
    }

    fn reached_oldest(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = &self.viewer else { return };
        if is_picture(id) {
            return;
        }
        if self.neighbours().0.is_none() && self.zoom.older_from.as_ref() != Some(id) {
            self.zoom.older_from = Some(id.clone());
            self.load_older(cx);
        }
    }

    /// Left and Right page through the chat's pictures; nothing animates on a key.
    pub(super) fn viewer_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.viewer.is_none() || self.zoom.zoomed_in() || !matches!(key, "left" | "right") {
            return false;
        }
        if self.turn(key == "right", cx) {
            self.zoom.drift.x.set(0.);
        }
        true
    }

    /// Reply or Forward from the viewer: closes it, then does what the message's menu does.
    fn viewer_act(&mut self, act: menu::Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.viewer.clone() else { return };
        self.dismiss_viewer(true, cx);
        self.act(&id, act, window, cx);
    }

    /// Closes the picture from a button or Android's back: back into its bubble when that is on
    /// screen, else fading. A key closes it at once.
    pub(super) fn dismiss_viewer(&mut self, animate: bool, cx: &mut Context<Self>) {
        let Some((id, full)) = &self.viewer else { return };
        let hero = self.zoom.home_of(id).is_some() && (full.is_some() || self.thumbs.contains_key(id));
        self.zoom.exit = if !animate || cx.reduce_motion() {
            Exit::Instant
        } else if hero {
            Exit::Hero
        } else {
            Exit::Fade
        };
        if !matches!(self.zoom.exit, Exit::Hero) {
            // No frame may come to show the thumbnail again.
            self.zoom.covers = None;
        }
        self.close_viewer(cx);
    }

    /// A finger let go of the picture to close it, at `vel` px/s: it lands in its bubble when that
    /// is on screen, else it moves on toward `dir` (up, down or nowhere: -1, 1, 0) while it fades.
    fn throw_away(&mut self, vel: f32, dir: f32, cx: &mut Context<Self>) {
        let Some((id, _)) = &self.viewer else { return };
        if cx.reduce_motion() {
            self.zoom.exit = Exit::Instant;
            self.zoom.covers = None;
            return self.close_viewer(cx);
        }
        let (from, now) = (self.zoom.drawn, Instant::now());
        let home = self.zoom.home_of(id).map(|h| Rect::of(h.bounds));
        let to = home.unwrap_or(Rect { y: self.zoom.screen.1 / 2. + dir * (self.zoom.screen.1 + from.h) / 2., ..from });
        let mut rect = [from.x, from.y, from.w, from.h].map(|v| glide(motion::THROW, v));
        for (g, (target, v)) in rect.iter_mut().zip([(to.x, 0.), (to.y, vel), (to.w, 0.), (to.h, 0.)]) {
            g.release(target, v, now);
        }
        self.zoom.exit = Exit::Throw(Box::new(Throw { rect, from, shade: self.zoom.shade, at: now, home: home.is_some() }));
        self.close_viewer(cx);
    }

    /// Follows a finger on a fitted picture from `at`, where it landed; on release it turns,
    /// closes or springs back, carrying the finger's speed.
    fn viewer_pull(&mut self, phase: TouchPhase, at: Point<Pixels>, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let now = Instant::now();
        match phase {
            TouchPhase::Started => {
                self.zoom.pull = Some(Pull { across: None, offset: 0., samples: VecDeque::new() });
                self.zoom.tap = None;
                // A finger catching a picture on its way back keeps the point it shrinks about.
                if !self.zoom.drift.y.running() {
                    self.zoom.grip = (f32::from(at.x) - self.zoom.screen.0 / 2., f32::from(at.y) - self.zoom.screen.1 / 2.);
                }
                self.pull_by(dx, dy, now);
            }
            TouchPhase::Moved => self.pull_by(dx, dy, now),
            TouchPhase::Ended => {
                let Some(mut pull) = self.zoom.pull.take() else { return };
                // A thrown picture closes the viewer while the fling's glide still comes.
                sheet::own_pan();
                // A finger that rested before lifting has no speed left.
                pull.samples.push_back((now, pull.offset));
                let speed = super::gallery::speed(&pull.samples);
                let (older, newer) = self.neighbours();
                let (width, height) = self.zoom.screen;
                match pull.across {
                    Some(true) => {
                        let drawn = give(pull.offset, older.is_some(), newer.is_some());
                        let turn = settle(drawn, speed, width, older.is_some(), newer.is_some());
                        let (from, vel) = if turn != Turn::Stay && self.turn(turn == Turn::Newer, cx) {
                            let page = width + PAGE_GAP;
                            (if turn == Turn::Newer { drawn + page } else { drawn - page }, speed)
                        } else {
                            if pull.offset > 0. && older.is_none() {
                                self.reached_oldest(cx);
                            }
                            (drawn, if drawn == pull.offset { speed } else { speed * EDGE_GIVE })
                        };
                        self.zoom.drift.x = glide(motion::SETTLE, from);
                        self.zoom.drift.x.release(0., vel, now);
                    }
                    Some(false) if dismisses(pull.offset, speed, height) => self.throw_away(speed, pull.offset.signum(), cx),
                    Some(false) => {
                        self.zoom.drift.y = glide(motion::RETURN, pull.offset);
                        self.zoom.drift.y.release(0., speed, now);
                    }
                    None => {}
                }
            }
            TouchPhase::Cancelled => {
                self.zoom.pull = None;
                self.zoom.drift.x.release(0., 0., now);
                self.zoom.drift.y.release(0., 0., now);
            }
        }
        cx.notify();
    }

    fn pull_by(&mut self, dx: f32, dy: f32, now: Instant) {
        let at = (self.zoom.drift.x.pos, self.zoom.drift.y.pos);
        let Some(pull) = &mut self.zoom.pull else { return };
        if pull.across.is_none() && (dx != 0. || dy != 0.) {
            let across = dx.abs() > dy.abs();
            pull.across = Some(across);
            // Caught mid-flight: the finger takes the picture from where it is drawn.
            pull.offset = if across { at.0 } else { at.1 };
        }
        let Some(across) = pull.across else { return };
        pull.offset += if across { dx } else { dy };
        pull.samples.push_back((now, pull.offset));
        if pull.samples.len() > 6 {
            pull.samples.pop_front();
        }
    }

    fn note(&mut self, text: impl Into<SharedString>, lasts: bool, cx: &mut Context<Self>) {
        self.zoom.epoch += 1;
        let epoch = self.zoom.epoch;
        self.zoom.note = Some((text.into(), epoch));
        if lasts {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(NOTE_MS)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.zoom.note.as_ref().is_some_and(|(_, e)| *e == epoch) {
                        this.zoom.note = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Save or Share for the picture on screen.
    fn export_viewed(&mut self, what: Export, cx: &mut Context<Self>) {
        if let Some((id, _)) = self.viewer.as_ref() {
            let id = id.clone();
            self.export_message(&id, what, cx);
        }
    }

    /// Downloads the original of a message's attachment, then saves or shares it.
    pub(super) fn export_message(&mut self, id: &str, what: Export, cx: &mut Context<Self>) {
        let Some(media) = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.clone()).filter(|media| !media.once) else { return };
        if self.zoom.working.is_some() {
            return;
        }
        if self.send(Cmd::Export(id.to_owned(), media, what), cx) {
            self.zoom.working = Some(what);
            self.note(if what == Export::Save { t!("Saving…") } else { t!("Preparing the file…") }, false, cx);
        }
    }

    pub(super) fn exported(&mut self, id: String, path: Option<PathBuf>, what: Export, cx: &mut Context<Self>) {
        self.zoom.working = None;
        let Some(path) = path else { return self.note(t!("Could not download the file."), true, cx) };
        let media = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.as_ref());
        let name = media.map(|m| m.name.clone()).or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "file".into());
        let mime = media.and_then(|m| m.mime.clone()).or_else(|| match media.map(|m| m.kind) {
            Some(MediaKind::Video) => Some("video/mp4".to_owned()),
            Some(MediaKind::Image | MediaKind::Sticker) => Some("image/jpeg".to_owned()),
            _ => None,
        });
        match what {
            Export::Share => {
                self.zoom.note = None;
                crate::share_file(&path, mime.as_deref());
                cx.notify();
            }
            Export::Save => {
                let saving = cx.background_spawn(async move { crate::save_file(&path, &name, mime.as_deref()) });
                cx.spawn(async move |this, cx| {
                    let done = saving.await;
                    let _ = this.update(cx, |this, cx| match done {
                        Ok(place) => this.note(t!("Saved to {place}", place = place), true, cx),
                        Err(e) => {
                            log::warn!("save: {e}");
                            this.note(t!("Could not save the file."), true, cx)
                        }
                    });
                })
                .detach();
            }
        }
    }

    fn viewer_tap(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.zoom.dragged > DRAG_SLOP {
            self.zoom.dragged = 0.;
            return;
        }
        let reduce = cx.reduce_motion();
        // Not window.request_animation_frame(): outside render it has no view and panics; the
        // render this notify causes keeps asking for frames while the zoom glides.
        if self.zoom.tapped(at, reduce) {
            cx.notify();
            return;
        }
        // A single tap shows or hides the bars, once no second tap has made it a double-tap.
        let stamp = self.zoom.tap.map(|(t, _)| t);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DOUBLE_TAP).await;
            let _ = this.update(cx, |this, cx| {
                if this.viewer.is_some() && this.zoom.tap.map(|(t, _)| t) == stamp {
                    this.zoom.tap = None;
                    this.zoom.bare = !this.zoom.bare;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn viewer_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if self.zoom.pinching {
            return;
        }
        let reduce = cx.reduce_motion();
        let delta = e.delta.pixel_delta(px(20.));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if e.modifiers.control || e.modifiers.platform {
            self.zoom.zoom_by(e.position, (dy * WHEEL_ZOOM).exp(), reduce);
        } else if self.zoom.pull.is_some() || (e.touch_phase == TouchPhase::Started && !self.zoom.zoomed_in()) {
            // A wheel and the glide after a fling have no start, so neither turns nor closes.
            return self.viewer_pull(e.touch_phase, e.position, dx, dy, cx);
        } else if self.zoom.zoomed_in() {
            self.viewer_pan(e.touch_phase, dx, dy, reduce);
        } else {
            return;
        }
        cx.notify();
    }

    /// Moves a zoomed picture. A finger past an edge meets resistance and springs back on release
    /// with its speed; a fling that reaches an edge hands its speed to the same spring. GPUI sends
    /// a fling's steps after the finger's `Ended`, as `Moved` with no `Started`, then an `Ended`.
    /// A wheel stops at the edge.
    fn viewer_pan(&mut self, phase: TouchPhase, dx: f32, dy: f32, reduce: bool) {
        let now = Instant::now();
        match phase {
            TouchPhase::Started => {
                let view = self.zoom.grab(reduce);
                let raw = (view.off.0 + self.zoom.drift.x.pos, view.off.1 + self.zoom.drift.y.pos);
                self.zoom.pan = Some(Pan { raw, xs: VecDeque::new(), ys: VecDeque::new() });
                self.zoom.coast = None;
                self.pan_by(dx, dy, now);
            }
            TouchPhase::Moved if self.zoom.pan.is_some() => self.pan_by(dx, dy, now),
            TouchPhase::Moved => match self.zoom.coast {
                Some(last) => self.coast_by(dx, dy, last, now),
                None => self.zoom.pan(dx, dy, reduce),
            },
            TouchPhase::Ended if self.zoom.pan.is_some() => {
                self.pan_by(dx, dy, now);
                let Some(pan) = self.zoom.pan.take() else { return };
                let speed = (super::gallery::speed(&pan.xs), super::gallery::speed(&pan.ys));
                for (g, v) in [(&mut self.zoom.drift.x, speed.0), (&mut self.zoom.drift.y, speed.1)] {
                    if g.pos != 0. {
                        *g = glide(motion::SETTLE, g.pos);
                        // Past the edge the picture moved a share of the finger's speed.
                        g.release(0., v * EDGE_GIVE, now);
                    }
                }
                self.zoom.coast = Some(now);
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.zoom.pan = None;
                self.zoom.coast = None;
                // A bounce already under way keeps its speed.
                for g in [&mut self.zoom.drift.x, &mut self.zoom.drift.y] {
                    if !g.running() && g.pos != 0. {
                        g.release(0., 0., now);
                    }
                }
            }
        }
    }

    fn pan_by(&mut self, dx: f32, dy: f32, now: Instant) {
        let (screen, fit) = (self.zoom.screen, self.zoom.fit);
        let Some(pan) = &mut self.zoom.pan else { return };
        pan.raw = (pan.raw.0 + dx, pan.raw.1 + dy);
        for (samples, at) in [(&mut pan.xs, pan.raw.0), (&mut pan.ys, pan.raw.1)] {
            samples.push_back((now, at));
            if samples.len() > 6 {
                samples.pop_front();
            }
        }
        let raw = pan.raw;
        let view = self.zoom.target();
        let (rx, ry) = reach(view.scale, screen, fit);
        let off = (raw.0.clamp(-rx, rx), raw.1.clamp(-ry, ry));
        self.zoom.view = Some(View { off, ..view });
        self.zoom.drift.x.hold(rubber(raw.0, rx, screen.0) - off.0);
        self.zoom.drift.y.hold(rubber(raw.1, ry, screen.1) - off.1);
    }

    /// One step of a fling after the finger lifted, `last` being when the previous one came.
    fn coast_by(&mut self, dx: f32, dy: f32, last: Instant, now: Instant) {
        let dt = now.saturating_duration_since(last).as_secs_f32().max(0.001);
        let mut view = self.zoom.target();
        let (rx, ry) = reach(view.scale, self.zoom.screen, self.zoom.fit);
        for (g, off, d, r) in [(&mut self.zoom.drift.x, &mut view.off.0, dx, rx), (&mut self.zoom.drift.y, &mut view.off.1, dy, ry)] {
            // An axis already springing back has left the fling.
            if g.running() {
                continue;
            }
            let next = *off + d;
            *off = next.clamp(-r, r);
            if next.abs() > r {
                *g = glide(motion::SETTLE, 0.);
                g.release(0., d / dt, now);
            }
        }
        self.zoom.view = Some(view);
        self.zoom.coast = Some(now);
    }

    fn viewer_pinch(&mut self, e: &PinchEvent, cx: &mut Context<Self>) {
        let reduce = cx.reduce_motion();
        let at = self.zoom.centred(e.position);
        match e.phase {
            TouchPhase::Started => {
                self.zoom.pinching = true;
                (self.zoom.pull, self.zoom.pan, self.zoom.coast) = (None, None, None);
                let mut from = self.zoom.grab(reduce);
                // Where a finger or a spring had moved it, the pinch takes it from.
                from.off = (from.off.0 + self.zoom.drift.x.pos, from.off.1 + self.zoom.drift.y.pos);
                self.zoom.drift = Drift::default();
                self.zoom.view = Some(from);
                self.zoom.pinch = Some(Pinch { raw: from.scale, from, at });
            }
            TouchPhase::Moved => match &mut self.zoom.pinch {
                Some(pinch) => {
                    pinch.raw *= 1. + e.delta;
                    let scale = resist(pinch.raw);
                    let k = scale / pinch.from.scale;
                    // The picture point that was under the fingers' midpoint stays under it as it moves.
                    let off = (at.0 - (pinch.at.0 - pinch.from.off.0) * k, at.1 - (pinch.at.1 - pinch.from.off.1) * k);
                    self.zoom.view = Some(View { scale, off });
                }
                // A trackpad pinch has no start.
                None => self.zoom.zoom_by(e.position, 1. + e.delta, reduce),
            },
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.zoom.pinching = false;
                if self.zoom.pinch.take().is_some() {
                    let view = self.zoom.target();
                    self.zoom.tap = None;
                    if e.phase == TouchPhase::Ended && view.scale < PINCH_CLOSE {
                        return self.throw_away(0., 0., cx);
                    }
                    let to = if view.scale > MAX_SCALE { zoomed_about(view, at, MAX_SCALE, self.zoom.screen, self.zoom.fit) } else { clamped(view, self.zoom.screen, self.zoom.fit) };
                    self.zoom.view = Some(to);
                    self.zoom.glide = Some((view, Instant::now()));
                }
            }
        }
        // A pinch is not a tap; the lift of the last finger must not close the picture.
        self.zoom.tap = None;
        cx.notify();
    }

    fn viewer_drag(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(last) = self.zoom.mouse else { return };
        if e.pressed_button != Some(MouseButton::Left) {
            self.zoom.mouse = None;
            return;
        }
        self.zoom.mouse = Some(e.position);
        let (dx, dy) = (f32::from(e.position.x - last.x), f32::from(e.position.y - last.y));
        self.zoom.dragged += dx.abs() + dy.abs();
        if self.zoom.zoomed_in() {
            self.zoom.pan(dx, dy, cx.reduce_motion());
            cx.notify();
        }
    }

    /// A picture full-screen. It flies out of its bubble and back into it when the bubble is on
    /// screen, else it fades; while it leaves it still takes every tap, so none reaches the chat.
    /// GPUI cannot scale an element, so the flight moves and sizes the picture's own box.
    pub(super) fn render_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let frame = self.zoom.frame;
        self.zoom.frame += 1;
        let live = self.viewer.is_some();
        if live {
            if let Some((_, Some(image))) = self.viewer_last.take() {
                self.released.push(image, cx);
                self.released.free(window, cx);
            }
        } else if self.viewer_last.is_none() {
            self.zoom.covers = None;
            return None;
        }
        let reduce = cx.reduce_motion();
        let now = Instant::now();
        let (id, full) = self.viewer.as_ref().or(self.viewer_last.as_ref())?;
        let (id, full) = (id.clone(), full.clone());
        let home = self.zoom.home_at(&id, frame).map(|h| (Rect::of(h.bounds), h.radius));

        let hero = if live { self.zoom.hero } else { matches!(self.zoom.exit, Exit::Hero) } && home.is_some();
        // A thrown picture is moved by its springs; anything else by the viewer's presence.
        let (p, done, throw) = match &mut self.zoom.exit {
            Exit::Throw(throw) if !live => {
                if let (true, Some((to, _))) = (throw.home, home) {
                    // The bubble moved: the chat scrolled under the picture.
                    for (g, target) in throw.rect.iter_mut().zip([to.x, to.y, to.w, to.h]) {
                        g.retarget(target);
                    }
                }
                let [x, y, w, h] = throw.rect.each_mut().map(|g| {
                    if reduce {
                        g.set(g.target());
                    }
                    g.step(now)
                });
                let rect = Rect { x, y, w, h };
                let target = Rect { x: throw.rect[0].target(), y: throw.rect[1].target(), w: throw.rect[2].target(), h: throw.rect[3].target() };
                let (p, done) = if throw.home {
                    (landed(rect, throw.from, target), throw.rect.iter().all(|g| !g.running()))
                } else {
                    let t = now.saturating_duration_since(throw.at).as_secs_f32() / motion::EXIT.as_secs_f32();
                    (motion::ease_out(t.min(1.)), reduce || t >= 1.)
                };
                (p, done, Some((rect, throw.shade, throw.home)))
            }
            exit => {
                let motion = match (live, exit) {
                    (true, _) if hero => Transition::new(motion::HERO_IN).easing(motion::EASE_OUT),
                    (true, _) => motion::enter(),
                    (false, Exit::Hero) => Transition::new(motion::HERO_OUT).easing(motion::EASE_OUT),
                    (false, Exit::Instant) => Transition::new(Duration::ZERO),
                    (false, _) => motion::exit(),
                };
                let shown = Presence::new("viewer", live).transition(motion).sample(window, cx);
                (shown.progress, !shown.should_render(), None)
            }
        };
        let lands = hero || throw.is_some_and(|(_, _, home)| home);
        if done {
            if !lands || self.zoom.landed {
                if let Some((_, Some(image))) = self.viewer_last.take() {
                    self.released.push(image, cx);
                    self.released.free(window, cx);
                }
                (self.zoom.covers, self.zoom.landed, self.zoom.exit) = (None, false, Exit::Fade);
                return None;
            }
            // Drawn in its bubble once more while the bubble's own picture comes back next frame.
            self.zoom.landed = true;
            window.request_animation_frame();
        } else if throw.is_some() {
            window.request_animation_frame();
        }
        self.zoom.covers = (live || (lands && !done)).then(|| id.clone());

        let palette = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let thumb = self.thumbs.get(&id).cloned().or_else(|| self.stand_in(&id));
        // The thumbnail stands in until the full picture arrives.
        let picture = full.clone().or_else(|| thumb.clone());
        if live && !self.focus.is_focused(window) {
            // Left and Right reach the app, not the text field under the picture.
            window.focus(&self.focus, cx);
        }

        let size = frame::viewport(window);
        let screen = (f32::from(size.width), f32::from(size.height));
        if let Some(image) = &picture
            && self.zoom.dims.as_ref().is_none_or(|(known, _)| *known != image.id)
        {
            self.zoom.dims = dimensions(image).map(|d| (image.id, d));
        }
        self.zoom.screen = screen;
        self.zoom.origin = frame::origin(window);
        self.zoom.fit = fitted(screen, self.zoom.dims.map(|(_, d)| d));
        let (view, gliding) = self.zoom.shown(reduce);
        if gliding {
            window.request_animation_frame();
        } else if self.zoom.glide.is_some() {
            self.zoom.glide = None;
        }

        let (older, newer) = if live { self.neighbours() } else { (None, None) };
        // History paged in while the picture is open brings a neighbour that is not asked for yet.
        if [&older, &newer].into_iter().flatten().any(|n| self.zoom.near.iter().all(|(k, _)| k != n)) {
            self.fill_near(cx);
        }
        // The finger's pull, 1:1; after it, a spring back to rest or to the picture turned to.
        if live && let Some(pull) = &self.zoom.pull {
            match pull.across {
                Some(true) => self.zoom.drift.x.hold(give(pull.offset, older.is_some(), newer.is_some())),
                Some(false) => self.zoom.drift.y.hold(pull.offset),
                None => {}
            }
        }
        let [x, y] = [&mut self.zoom.drift.x, &mut self.zoom.drift.y].map(|g| {
            if reduce {
                g.set(g.target());
            }
            g.step(now)
        });
        if self.zoom.drift.x.running() || self.zoom.drift.y.running() {
            window.request_animation_frame();
        }
        // Fitted, a pull up or down shrinks the picture and uncovers the chat; zoomed, the drift is
        // how far it is past an edge.
        let fitted_now = !self.zoom.zoomed_in();
        let s = if fitted_now { shrink(y, screen.1) } else { 1. };
        let fade = if fitted_now { (y.abs() / (screen.1 * 0.4)).min(1.) } else { 0. };
        let rest = placed(view, screen, self.zoom.fit, (x, y), self.zoom.grip, s);
        let shade = (1. - fade) * pinch_shade(view.scale);
        // Where the picture is, its corners, the backdrop behind it, and the whole viewer's opacity.
        let (rect, radius, backdrop, opacity) = match (throw, home) {
            (Some((rect, from, true)), Some((_, r))) => (rect, r * p, from * (1. - p), 1.),
            (Some((rect, from, _)), _) => (rect, 0., from, 1. - p),
            (None, Some((thumb_rect, r))) if hero => (thumb_rect.lerp(rest, p), r * (1. - p), shade * p, 1.),
            (None, _) => (rest, 0., shade, p),
        };
        (self.zoom.drawn, self.zoom.shade) = (rect, backdrop);

        let serial = SharedString::from(format!("viewer-{}", self.zoom.serial));
        let bars = transition((serial.clone(), "bars"), if self.zoom.bare { 0. } else { 1. }, if self.zoom.bare { motion::exit() } else { motion::enter() }, window, cx);
        let bars_in = Presence::new((serial.clone(), "bars-in"), true).transition(motion::enter().delay(BARS_DELAY)).sample(window, cx).progress;
        let leaving = if throw.is_some() { 0. } else if hero { p } else { 1. };
        let chrome = bars * bars_in * leaving * (1. - (fade * 3.).min(1.));
        let interactive = live && !self.zoom.bare;
        let sharp = transition(
            (SharedString::from(format!("viewer-{}-{id}", self.zoom.serial)), "sharp"),
            if full.is_some() { 1. } else { 0. },
            Transition::new(motion::SHARPEN).easing(motion::EASE_OUT),
            window,
            cx,
        );
        if let Some((text, _)) = &self.zoom.note {
            self.zoom.note_text = text.clone();
        }
        let noting = self.zoom.note.is_some();
        let note = Presence::new((serial.clone(), "note"), noting).transition(if noting { motion::enter() } else { motion::exit() }).sample(window, cx);

        let page = screen.0 + PAGE_GAP;
        let side = |near: &Option<String>, shift: f32| -> Option<AnyElement> {
            let near = near.as_ref()?;
            let image = self.zoom.near.iter().find(|(k, _)| k == near).and_then(|(_, image)| image.clone()).or_else(|| self.thumbs.get(near).cloned())?;
            Some(img(image).absolute().left(px(x + shift)).top_0().w(px(screen.0)).h(px(screen.1)).object_fit(ObjectFit::Contain).into_any_element())
        };
        let sides: Vec<AnyElement> = if fitted_now && x.abs() > 0.5 { [side(&older, -page), side(&newer, page)].into_iter().flatten().collect() } else { Vec::new() };

        let m = self.msgs.iter().find(|m| m.id == id);
        let who = m.map(|m| if m.own { t!("You").to_owned() } else { m.sender.clone() }).or_else(|| self.zoom.title.clone());
        let when = m.map(|m| format!("{}, {}", day_label(m.ts), clock(m.ts)));
        let caption = m.map(|m| m.body.trim()).filter(|body| !body.is_empty()).map(str::to_owned);
        let replies = m.is_some_and(|m| m.id.starts_with('$') && !m.deleted);
        let once = m.and_then(|m| m.media.as_ref()).is_some_and(|media| media.once);
        // A profile picture is no message's file: nothing to save or share.
        let file = m.is_some() && !once;
        let forwards = !once && m.is_some_and(forward::forwardable);

        let shade = gpui_kit::white().opacity(0.12);
        let scrim = gpui_kit::black().opacity(0.55);
        let working = self.zoom.working.is_some();
        let action = |id: &'static str, icon: IconName, label: &'static str, busy: bool| {
            v_flex()
                .id(id)
                .min_w(px(TAP + 20.))
                .h(px(ACTIONS_H))
                .items_center()
                .justify_center()
                .gap(px(4.))
                .rounded(px(12.))
                .cursor_pointer()
                .opacity(if busy { 0.5 } else { 1. })
                .when(!gpui_kit::is_mobile(), |el| el.hover(|s| s.bg(shade)))
                .active(|s| s.bg(shade))
                .child(Icon::new(icon).size_6())
                .child(div().text_size(fs(12.)).child(label))
        };
        let top_bar = h_flex()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .pt(px(top + 4.))
            .pb(px(4.))
            .px(px(GUTTER - 6.))
            .gap(px(6.))
            .bg(scrim)
            .opacity(chrome)
            .child(
                div()
                    .id("viewer-close")
                    .size(px(TAP))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .when(!gpui_kit::is_mobile(), |el| el.hover(|s| s.bg(shade)))
                    .active(|s| s.bg(shade))
                    .child(Icon::new(IconName::ArrowLeft).size_6())
                    .when(interactive, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.dismiss_viewer(true, cx);
                        }))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .children(who.map(|who| div().truncate().text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD).child(who)))
                    .children(when.map(|when| div().truncate().text_size(fs(13.)).text_color(gpui_kit::white().opacity(0.7)).child(when))),
            );
        let actions = h_flex()
            .justify_around()
            .px(px(GUTTER / 2.))
            .when(replies, |el| {
                el.child(action("viewer-reply", IconName::Reply, t!("Reply"), false).when(interactive, |el| {
                    el.on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.viewer_act(menu::Act::Reply, window, cx);
                    }))
                }))
            })
            .when(forwards, |el| {
                el.child(action("viewer-forward", IconName::Forward, t!("Forward"), false).when(interactive, |el| {
                    el.on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.viewer_act(menu::Act::Forward, window, cx);
                    }))
                }))
            })
            .when(crate::can_save() && file, |el| {
                el.child(action("viewer-save", IconName::Download, t!("Save"), working).when(interactive, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.export_viewed(Export::Save, cx);
                    }))
                }))
            })
            .when(crate::can_share() && file, |el| {
                el.child(action("viewer-share", IconName::Share2, t!("Share"), working).when(interactive, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.export_viewed(Export::Share, cx);
                    }))
                }))
            });
        let bottom_bar = v_flex()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .gap(px(12.))
            .when(note.should_render(), |el| {
                el.child(div().relative().top(px(NOTE_RISE * (1. - note.progress))).opacity(note.progress).flex().justify_center().px(px(GUTTER)).child(
                    div()
                        .max_w(px(screen.0 - 2. * GUTTER))
                        .px(px(GAP))
                        .py(px(10.))
                        .rounded(px(RADIUS))
                        .bg(palette.raised)
                        .text_color(palette.text)
                        .text_size(fs(14.))
                        .child(self.zoom.note_text.clone()),
                ))
            })
            // A profile picture has no caption and no actions: no empty bar under it.
            .when(m.is_some(), |el| {
                el.child(
                    v_flex()
                        .pt(px(8.))
                        .pb(px(bottom + 4.))
                        .gap(px(4.))
                        .bg(scrim)
                        .opacity(chrome)
                        .children(caption.map(|caption| div().px(px(GUTTER)).py(px(4.)).line_clamp(4).text_ellipsis().text_size(fs(15.)).line_height(fs(21.)).child(caption)))
                        .child(actions),
                )
            });

        // A sticker keeps its own shape; a photo's box has its shape once it lands, and in between
        // `Cover` crops it as the bubble does.
        let sticker = m.and_then(|m| m.media.as_ref()).is_some_and(|media| media.kind == MediaKind::Sticker);
        let cover = !sticker && self.zoom.dims.is_some();
        let layer = |image: Arc<Image>, alpha: f32| {
            img(image)
                .absolute()
                .left(px(rect.x - rect.w / 2.))
                .top(px(rect.y - rect.h / 2.))
                .w(px(rect.w))
                .h(px(rect.h))
                .rounded(px(if sticker { 0. } else { radius }))
                .object_fit(if cover { ObjectFit::Cover } else { ObjectFit::Contain })
                .opacity(alpha)
                .into_any_element()
        };
        // The full picture fades in over the thumbnail when it arrives after opening.
        let layers: Vec<AnyElement> = match (full, thumb) {
            (Some(full), Some(thumb)) if sharp < 1. => vec![layer(thumb, 1.), layer(full, sharp)],
            (Some(image), _) | (None, Some(image)) => vec![layer(image, 1.)],
            (None, None) => Vec::new(),
        };
        Some(
            div()
                .id("viewer")
                .absolute()
                .inset_0()
                .overflow_hidden()
                .text_color(gpui_kit::white())
                .opacity(opacity)
                .child(div().absolute().inset_0().bg(gpui_kit::black()).opacity(backdrop))
                // Until the picture is gone, through every way out: a tap meant for it must not
                // reach the chat, whose back arrow sits under the close arrow.
                .occlude()
                .when(live, |el| {
                    el.on_click(cx.listener(|this, e: &ClickEvent, _, cx| this.viewer_tap(e.position(), cx)))
                        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| this.viewer_wheel(e, cx)))
                        .on_pinch(cx.listener(|this, e: &PinchEvent, _, cx| this.viewer_pinch(e, cx)))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, _| {
                            this.zoom.mouse = Some(e.position);
                            this.zoom.dragged = 0.;
                        }))
                        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| this.viewer_drag(e, cx)))
                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.zoom.mouse = None))
                })
                .children(sides)
                .children(layers)
                .when(live && picture.is_none(), |el| el.child(div().absolute().inset_0().flex().items_center().justify_center().child(t!("Loading…"))))
                .when(chrome > 0., |el| el.child(top_bar))
                .child(bottom_bar)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EDGE_GIVE, FIT, MAX_SCALE, OVERPAN, PINCH_MIN, Rect, Turn, View, beside, clamped, dismisses, fitted, give, landed, pictures, pinch_shade, placed, reach, resist, rubber, settle, shrink,
        zoomed_about,
    };
    use crate::backend::MediaKind;

    const PHONE: (f32, f32) = (400., 900.);

    #[test]
    fn a_picture_is_fitted_inside_the_screen() {
        assert_eq!(fitted(PHONE, Some((4000., 3000.))), (400., 300.));
        assert_eq!(fitted(PHONE, Some((1000., 4000.))), (225., 900.));
        assert_eq!(fitted(PHONE, None), PHONE);
    }

    #[test]
    fn a_side_still_smaller_than_the_screen_cannot_move() {
        let fit = fitted(PHONE, Some((4000., 3000.)));
        assert_eq!(reach(1., PHONE, fit), (0., 0.));
        // 300 px tall at 2.5x is 750 px: still inside 900, so no vertical travel; 1000 px wide is not.
        assert_eq!(reach(2.5, PHONE, fit), (300., 0.));
    }

    #[test]
    fn the_edge_of_the_picture_stops_at_the_edge_of_the_screen() {
        let fit = PHONE;
        let far = clamped(View { scale: 2., off: (999., -999.) }, PHONE, fit);
        assert_eq!(far.off, (200., -450.));
        assert_eq!(clamped(View { scale: 99., off: (0., 0.) }, PHONE, fit).scale, MAX_SCALE);
        assert_eq!(clamped(View { scale: 0.2, off: (50., 50.) }, PHONE, fit), FIT);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_finger_still() {
        let at = (100., 200.);
        let v = zoomed_about(FIT, at, 2.5, PHONE, PHONE);
        // The picture point under `at` is (at - off) / scale; it must be the same before and after.
        assert!(((at.0 - v.off.0) / v.scale - at.0).abs() < 1e-3);
        assert!(((at.1 - v.off.1) / v.scale - at.1).abs() < 1e-3);
        assert_eq!(zoomed_about(v, at, 1., PHONE, PHONE), FIT);
    }

    #[test]
    fn only_images_and_stickers_still_there_are_browsed_in_order() {
        let msgs = [
            ("$a", Some(MediaKind::Image), false),
            ("$b", None, false),
            ("$c", Some(MediaKind::Video), false),
            ("$d", Some(MediaKind::Sticker), false),
            ("$e", Some(MediaKind::Image), true),
            ("$f", Some(MediaKind::File), false),
            ("$g", Some(MediaKind::Image), false),
        ];
        assert_eq!(pictures(msgs), ["$a", "$d", "$g"]);
    }

    #[test]
    fn only_the_neighbours_on_each_side_are_kept() {
        let set = ["$a", "$b", "$c", "$d"];
        assert_eq!(beside(&set, "$b"), (Some("$a"), Some("$c")));
        assert_eq!(beside(&set, "$a"), (None, Some("$b")));
        assert_eq!(beside(&set, "$d"), (Some("$c"), None));
        assert_eq!(beside(&set, "$x"), (None, None), "a picture not in the chat has no neighbours");
        assert_eq!(beside(&["$a"], "$a"), (None, None));
    }

    #[test]
    fn a_long_pull_or_a_flick_turns_a_short_slow_one_stays() {
        let width = 400.;
        assert_eq!(settle(-250., 0., width, true, true), Turn::Newer);
        assert_eq!(settle(250., 0., width, true, true), Turn::Older);
        assert_eq!(settle(-150., 0., width, true, true), Turn::Stay);
        assert_eq!(settle(-40., -900., width, true, true), Turn::Newer, "a flick turns");
        assert_eq!(settle(-40., 900., width, true, true), Turn::Stay, "a finger turning back cancels");
        assert_eq!(settle(-300., -900., width, true, false), Turn::Stay, "nothing newer to turn to");
        assert_eq!(settle(0., -900., width, true, true), Turn::Stay);
    }

    #[test]
    fn past_the_last_picture_the_pull_meets_resistance() {
        assert_eq!(give(100., true, false), 100.);
        assert_eq!(give(-100., true, false), -100. * EDGE_GIVE);
        assert_eq!(give(100., false, true), 100. * EDGE_GIVE);
    }

    #[test]
    fn a_far_or_fast_pull_up_or_down_closes() {
        let height = 900.;
        assert!(dismisses(200., 0., height));
        assert!(dismisses(-200., 0., height));
        assert!(!dismisses(60., 100., height), "a short slow pull springs back");
        assert!(dismisses(40., 1200., height), "a flick closes");
        assert!(!dismisses(40., -1200., height), "a flick back toward rest does not");
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn the_flight_from_the_bubble_passes_through_every_rect_between() {
        let (a, b) = (Rect { x: 10., y: 20., w: 100., h: 50. }, Rect { x: 200., y: 450., w: 400., h: 300. });
        assert_eq!(a.lerp(b, 0.), a);
        assert_eq!(a.lerp(b, 1.), b);
        assert_eq!(a.lerp(b, 0.5), Rect { x: 105., y: 235., w: 250., h: 175. });
    }

    #[test]
    fn a_pulled_picture_shrinks_to_seventy_percent_about_the_finger() {
        let h = PHONE.1;
        assert_eq!(shrink(0., h), 1.);
        assert!(near(shrink(0.15 * h, h), 0.925), "at the closing threshold");
        assert!(near(shrink(-2. * h, h), 0.7), "no smaller, up or down");
        // The point the finger took stays under it: grab (100, 200) from the centre, pulled 90 down.
        let (grab, dy) = ((100., 200.), 90.);
        let s = shrink(dy, h);
        let r = placed(FIT, PHONE, PHONE, (0., dy), grab, s);
        let finger = (PHONE.0 / 2. + grab.0, PHONE.1 / 2. + grab.1 + dy);
        // The picture point under the finger, as a share of the picture from its centre, is unchanged.
        assert!(near((finger.0 - r.x) / r.w, grab.0 / PHONE.0));
        assert!(near((finger.1 - r.y) / r.h, grab.1 / PHONE.1));
        assert!(near(r.w, PHONE.0 * s));
    }

    #[test]
    fn past_a_zoomed_edge_the_picture_gives_a_little_and_no_more() {
        assert_eq!(rubber(50., 100., 400.), 50.);
        assert_eq!(rubber(-100., 100., 400.), -100.);
        assert!(near(rubber(150., 100., 400.), 100. + 50. * EDGE_GIVE));
        assert!(near(rubber(-1000., 100., 400.), -(100. + 400. * OVERPAN)), "capped at a share of the screen");
    }

    #[test]
    fn a_pinch_past_the_limits_meets_resistance() {
        assert_eq!(resist(3.), 3.);
        assert!(near(resist(MAX_SCALE * 2.), MAX_SCALE * 2f32.powf(0.35)));
        assert!(resist(MAX_SCALE * 2.) < MAX_SCALE * 1.3);
        assert!(near(resist(0.64), 0.8));
        assert_eq!(resist(0.1), PINCH_MIN, "never smaller");
        assert_eq!(pinch_shade(1.2), 1.);
        assert!(near(pinch_shade(0.8), 0.5));
        assert_eq!(pinch_shade(PINCH_MIN), 0.);
    }

    #[test]
    fn a_thrown_picture_lands_by_its_longest_travel() {
        let from = Rect { x: 200., y: 600., w: 400., h: 300. };
        let to = Rect { x: 300., y: 200., w: 120., h: 90. };
        assert_eq!(landed(from, from, to), 0.);
        assert_eq!(landed(to, from, to), 1.);
        assert!(near(landed(Rect { y: 400., ..from }, from, to), 0.5));
        assert_eq!(landed(Rect { y: 900., ..from }, from, to), 0., "flung away from its bubble first");
        // Level with its bubble: the size tells how far it is.
        let level = Rect { y: 600., ..to };
        assert!(near(landed(Rect { w: 260., ..from }, from, level), 0.5));
    }
}
