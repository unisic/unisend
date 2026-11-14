//! The full-screen picture: pinch, double-tap or ctrl+wheel to zoom, drag to pan, and Save and
//! Share. GPUI cannot scale an element, so a zoomed picture is laid out larger than the screen
//! inside an `overflow: hidden` box and moved by an offset that is kept inside the picture's edges.
//!
//! A fitted picture follows a finger: sideways to the chat's previous or next picture, up or down
//! to close. A tap shows or hides the bars with the sender and the actions.

use std::collections::VecDeque;
use std::io::Cursor;

use super::*;
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
    fit: (f32, f32),
    dims: Option<(u64, (f32, f32))>,
    working: Option<Export>,
    note: Option<(SharedString, u64)>,
    epoch: u64,
    /// A finger moving the fitted picture.
    pull: Option<Pull>,
    /// Where the springs drew the picture last frame, so a finger can catch it mid-flight.
    at: (f32, f32),
    /// A sideways offset drawn for one frame without travel: where the picture just turned to sits.
    pin: Option<f32>,
    /// Where a picture flung away keeps moving while it fades.
    fall: f32,
    /// The bars are hidden.
    bare: bool,
    /// The pictures beside the one on screen, loaded or on their way: at most one each side.
    near: Vec<(String, Option<Arc<Image>>)>,
    /// The oldest loaded picture older history was last asked for from.
    older_from: Option<String>,
    /// Counts openings, so springs and fades start afresh with each picture opened.
    serial: u64,
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

    fn zoom_by(&mut self, at: Point<Pixels>, factor: f32, reduce_motion: bool) {
        let now = self.grab(reduce_motion);
        let at = (f32::from(at.x) - self.screen.0 / 2., f32::from(at.y) - self.screen.1 / 2.);
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
            let centred = (f32::from(at.x) - self.screen.0 / 2., f32::from(at.y) - self.screen.1 / 2.);
            zoomed_about(from, centred, DOUBLE_TAP_SCALE, self.screen, self.fit)
        };
        self.view = Some(to);
        self.glide = Some((from, now));
        true
    }

    fn zoomed_in(&self) -> bool {
        self.target().scale > 1.01
    }
}

fn dimensions(image: &Image) -> Option<(f32, f32)> {
    let (w, h) = image::ImageReader::new(Cursor::new(&image.bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some((w as f32, h as f32))
}

impl BeeperApp {
    pub(super) fn open_zoom(&mut self, cx: &mut Context<Self>) {
        self.drop_near(cx);
        self.zoom = Zoom { serial: self.zoom.serial + 1, ..Zoom::default() };
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
                image.remove_asset(cx);
            }
        }
    }

    /// The pictures before and after the one on screen, among those of the open chat.
    fn neighbours(&self) -> (Option<String>, Option<String>) {
        let Some((id, _)) = &self.viewer else { return (None, None) };
        let set = pictures(self.msgs.iter().filter(|m| !self.is_hidden(&m.id)).map(|m| (m.id.as_str(), m.media.as_ref().map(|media| media.kind), m.deleted)));
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
                image.remove_asset(cx);
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
            self.zoom.pin = Some(0.);
        }
        true
    }

    /// Reply or Forward from the viewer: closes it, then does what the message's menu does.
    fn viewer_act(&mut self, act: menu::Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, _)) = self.viewer.clone() else { return };
        self.close_viewer(cx);
        self.act(&id, act, window, cx);
    }

    /// Follows a finger on a fitted picture; on release it turns, closes or springs back.
    fn viewer_pull(&mut self, phase: TouchPhase, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let now = Instant::now();
        match phase {
            TouchPhase::Started => {
                self.zoom.pull = Some(Pull { across: None, offset: 0., samples: VecDeque::new() });
                self.zoom.tap = None;
                self.pull_by(dx, dy, now);
            }
            TouchPhase::Moved => self.pull_by(dx, dy, now),
            TouchPhase::Ended => {
                let Some(mut pull) = self.zoom.pull.take() else { return };
                // A finger that rested before lifting has no speed left.
                pull.samples.push_back((now, pull.offset));
                let speed = super::gallery::speed(&pull.samples);
                let (older, newer) = self.neighbours();
                let (width, height) = self.zoom.screen;
                match pull.across {
                    Some(true) => {
                        let drawn = give(pull.offset, older.is_some(), newer.is_some());
                        let turn = settle(drawn, speed, width, older.is_some(), newer.is_some());
                        if turn != Turn::Stay && self.turn(turn == Turn::Newer, cx) {
                            let page = width + PAGE_GAP;
                            self.zoom.pin = Some(if turn == Turn::Newer { drawn + page } else { drawn - page });
                        } else if pull.offset > 0. && older.is_none() {
                            self.reached_oldest(cx);
                        }
                    }
                    Some(false) if dismisses(pull.offset, speed, height) => {
                        self.zoom.fall = pull.offset + pull.offset.signum() * height * 0.25;
                        self.close_viewer(cx);
                    }
                    _ => {}
                }
            }
            TouchPhase::Cancelled => self.zoom.pull = None,
        }
        cx.notify();
    }

    fn pull_by(&mut self, dx: f32, dy: f32, now: Instant) {
        let at = self.zoom.at;
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
        let Some(media) = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.clone()) else { return };
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
            return self.viewer_pull(e.touch_phase, dx, dy, cx);
        } else if self.zoom.zoomed_in() {
            self.zoom.pan(dx, dy, reduce);
        } else {
            return;
        }
        cx.notify();
    }

    fn viewer_pinch(&mut self, e: &PinchEvent, cx: &mut Context<Self>) {
        let reduce = cx.reduce_motion();
        match e.phase {
            TouchPhase::Started => {
                self.zoom.pinching = true;
                self.zoom.pull = None;
                self.zoom.grab(reduce);
            }
            TouchPhase::Moved => {
                // A trackpad pinch has no start.
                self.zoom.zoom_by(e.position, 1. + e.delta, reduce);
            }
            TouchPhase::Ended | TouchPhase::Cancelled => self.zoom.pinching = false,
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

    /// A picture full-screen: fades in, and fades out quicker without taking taps while it does.
    /// No zoom from the thumbnail: GPUI cannot scale an element.
    pub(super) fn render_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.viewer.is_some();
        if live {
            if let Some((_, Some(image))) = self.viewer_last.take() {
                image.remove_asset(cx);
            }
        } else if self.viewer_last.is_none() {
            return None;
        }
        let shown = Presence::new("viewer", live).transition(if live { motion::enter() } else { motion::exit() }).sample(window, cx);
        if !shown.should_render() {
            if let Some((_, Some(image))) = self.viewer_last.take() {
                image.remove_asset(cx);
            }
            return None;
        }
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let (id, full) = self.viewer.as_ref().or(self.viewer_last.as_ref())?;
        let id = id.clone();
        // The thumbnail stands in until the full picture arrives.
        let picture = full.clone().or_else(|| self.thumbs.get(&id).cloned());
        if live && !self.focus.is_focused(window) {
            // Left and Right reach the app, not the text field under the picture.
            window.focus(&self.focus, cx);
        }

        let size = window.viewport_size();
        let screen = (f32::from(size.width), f32::from(size.height));
        if let Some(image) = &picture
            && self.zoom.dims.as_ref().is_none_or(|(known, _)| *known != image.id)
        {
            self.zoom.dims = dimensions(image).map(|d| (image.id, d));
        }
        self.zoom.screen = screen;
        self.zoom.fit = fitted(screen, self.zoom.dims.map(|(_, d)| d));
        let (view, gliding) = self.zoom.shown(cx.reduce_motion());
        if gliding {
            window.request_animation_frame();
        } else if self.zoom.glide.is_some() {
            self.zoom.glide = None;
        }
        let (w, h) = (screen.0 * view.scale, screen.1 * view.scale);

        let (older, newer) = if live { self.neighbours() } else { (None, None) };
        // History paged in while the picture is open brings a neighbour that is not asked for yet.
        if [&older, &newer].into_iter().flatten().any(|n| self.zoom.near.iter().all(|(k, _)| k != n)) {
            self.fill_near(cx);
        }
        // The finger's pull, 1:1; after it, a spring back to rest, to the picture turned to, or on
        // past the edge while a flung picture fades.
        let pull = if live { self.zoom.pull.as_ref().and_then(|pull| pull.across.map(|across| (across, pull.offset))) } else { None };
        let across = pull.filter(|(across, _)| *across).map(|(_, offset)| give(offset, older.is_some(), newer.is_some()));
        let down = pull.filter(|(across, _)| !across).map(|(_, offset)| offset);
        let serial = SharedString::from(format!("viewer-{}", self.zoom.serial));
        let x = match self.zoom.pin.take() {
            Some(pin) => {
                window.request_animation_frame();
                spring((serial.clone(), "x"), pin, motion::RELEASE.with_travel(false), window, cx)
            }
            None => spring((serial.clone(), "x"), across.unwrap_or(0.), motion::RELEASE.with_travel(across.is_none()), window, cx),
        };
        let y = spring((serial.clone(), "y"), if live { down.unwrap_or(0.) } else { self.zoom.fall }, motion::RELEASE.with_travel(down.is_none()), window, cx);
        self.zoom.at = (x, y);
        let fade = (y.abs() / (screen.1 * 0.4)).min(1.);
        let bars = transition((serial.clone(), "bars"), if self.zoom.bare { 0. } else { 1. }, if self.zoom.bare { motion::exit() } else { motion::enter() }, window, cx);
        let chrome = bars * (1. - (fade * 3.).min(1.));
        let interactive = live && !self.zoom.bare;

        let page = screen.0 + PAGE_GAP;
        let side = |near: &Option<String>, shift: f32| -> Option<AnyElement> {
            let near = near.as_ref()?;
            let image = self.zoom.near.iter().find(|(k, _)| k == near).and_then(|(_, image)| image.clone()).or_else(|| self.thumbs.get(near).cloned())?;
            Some(img(image).absolute().left(px(x + shift)).top_0().w(px(screen.0)).h(px(screen.1)).object_fit(ObjectFit::Contain).into_any_element())
        };
        let sides: Vec<AnyElement> = if x.abs() > 0.5 { [side(&older, -page), side(&newer, page)].into_iter().flatten().collect() } else { Vec::new() };

        let m = self.msgs.iter().find(|m| m.id == id);
        let who = m.map(|m| if m.own { t!("You").to_owned() } else { m.sender.clone() });
        let when = m.map(|m| format!("{}, {}", day_label(m.ts), clock(m.ts)));
        let caption = m.map(|m| m.body.trim()).filter(|body| !body.is_empty()).map(str::to_owned);
        let replies = m.is_some_and(|m| m.id.starts_with('$') && !m.deleted);
        let forwards = m.is_some_and(forward::forwardable);

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
                            this.close_viewer(cx);
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
            .when(crate::can_save(), |el| {
                el.child(action("viewer-save", IconName::Download, t!("Save"), working).when(interactive, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.export_viewed(Export::Save, cx);
                    }))
                }))
            })
            .when(crate::can_share(), |el| {
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
            .children(self.zoom.note.as_ref().map(|(text, _)| {
                div().flex().justify_center().px(px(GUTTER)).child(
                    div().max_w(px(screen.0 - 2. * GUTTER)).px(px(GAP)).py(px(10.)).rounded(px(RADIUS)).bg(p.raised).text_color(p.text).text_size(fs(14.)).child(text.clone()),
                )
            }))
            .child(
                v_flex()
                    .pt(px(8.))
                    .pb(px(bottom + 4.))
                    .gap(px(4.))
                    .bg(scrim)
                    .opacity(chrome)
                    .children(caption.map(|caption| div().px(px(GUTTER)).py(px(4.)).line_clamp(4).text_ellipsis().text_size(fs(15.)).line_height(fs(21.)).child(caption)))
                    .child(actions),
            );

        Some(
            div()
                .id("viewer")
                .absolute()
                .inset_0()
                .overflow_hidden()
                .text_color(gpui_kit::white())
                .opacity(shown.progress)
                .child(div().absolute().inset_0().bg(gpui_kit::black()).opacity(1. - fade))
                .when(live, |el| {
                    el.occlude()
                        .on_click(cx.listener(|this, e: &ClickEvent, _, cx| this.viewer_tap(e.position(), cx)))
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
                .map(|el| match picture {
                    Some(image) => el.child(
                        img(image)
                            .absolute()
                            .left(px((screen.0 - w) / 2. + view.off.0 + x))
                            .top(px((screen.1 - h) / 2. + view.off.1 + y))
                            .w(px(w))
                            .h(px(h))
                            .object_fit(ObjectFit::Contain),
                    ),
                    None if live => el.child(div().absolute().inset_0().flex().items_center().justify_center().child(t!("Loading…"))),
                    None => el,
                })
                .when(chrome > 0., |el| el.child(top_bar))
                .child(bottom_bar)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{EDGE_GIVE, FIT, MAX_SCALE, Turn, View, beside, clamped, dismisses, fitted, give, pictures, reach, settle, zoomed_about};
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
}
