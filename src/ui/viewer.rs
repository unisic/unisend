//! The full-screen picture: pinch, double-tap or ctrl+wheel to zoom, drag to pan, and Save and
//! Share. GPUI cannot scale an element, so a zoomed picture is laid out larger than the screen
//! inside an `overflow: hidden` box and moved by an offset that is kept inside the picture's edges.

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
    pub(super) fn open_zoom(&mut self) {
        self.zoom = Zoom::default();
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

    fn viewer_tap(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.zoom.dragged > DRAG_SLOP {
            self.zoom.dragged = 0.;
            return;
        }
        let reduce = cx.reduce_motion();
        if self.zoom.tapped(at, reduce) {
            window.request_animation_frame();
            cx.notify();
            return;
        }
        // A tap on a fitted picture closes it, unless a second tap follows.
        let stamp = self.zoom.tap.map(|(t, _)| t);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DOUBLE_TAP).await;
            let _ = this.update(cx, |this, cx| {
                if this.zoom.tap.map(|(t, _)| t) == stamp && !this.zoom.zoomed_in() {
                    this.close_viewer(cx);
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
        let (top, _) = crate::insets();
        let (id, full) = self.viewer.as_ref().or(self.viewer_last.as_ref())?;
        // The thumbnail stands in until the full picture arrives.
        let picture = full.clone().or_else(|| self.thumbs.get(id).cloned());

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

        let button = |id: &'static str, icon: IconName, busy: bool| {
            div()
                .id(id)
                .size(px(TAP))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .opacity(if busy { 0.5 } else { 1. })
                .hover(|s| s.bg(gpui_kit::white().opacity(0.12)))
                .child(Icon::new(icon).size_6())
        };
        let working = self.zoom.working;
        let bar = h_flex()
            .absolute()
            .top(px(top + 4.))
            .right(px(GUTTER - 6.))
            .when(live && crate::can_share(), |el| {
                el.child(button("viewer-share", IconName::Share2, working.is_some()).on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.export_viewed(Export::Share, cx);
                })))
            })
            .when(live && crate::can_save(), |el| {
                el.child(button("viewer-save", IconName::Download, working.is_some()).on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.export_viewed(Export::Save, cx);
                })))
            })
            .child(button("viewer-close", IconName::X, false).on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.close_viewer(cx);
            })));

        Some(
            div()
                .id("viewer")
                .absolute()
                .inset_0()
                .overflow_hidden()
                .bg(gpui_kit::black())
                .text_color(gpui_kit::white())
                .opacity(shown.progress)
                .when(live, |el| {
                    el.occlude()
                        .on_click(cx.listener(|this, e: &ClickEvent, window, cx| this.viewer_tap(e.position(), window, cx)))
                        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| this.viewer_wheel(e, cx)))
                        .on_pinch(cx.listener(|this, e: &PinchEvent, _, cx| this.viewer_pinch(e, cx)))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, _| {
                            this.zoom.mouse = Some(e.position);
                            this.zoom.dragged = 0.;
                        }))
                        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| this.viewer_drag(e, cx)))
                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.zoom.mouse = None))
                })
                .map(|el| match picture {
                    Some(image) => el.child(
                        img(image)
                            .absolute()
                            .left(px((screen.0 - w) / 2. + view.off.0))
                            .top(px((screen.1 - h) / 2. + view.off.1))
                            .w(px(w))
                            .h(px(h))
                            .object_fit(ObjectFit::Contain),
                    ),
                    None if live => el.child(div().size_full().flex().items_center().justify_center().child(t!("Loading…"))),
                    None => el,
                })
                .child(bar)
                .children(self.zoom.note.as_ref().map(|(text, _)| {
                    div().absolute().left_0().right_0().bottom(px(crate::insets().1 + 24.)).flex().justify_center().child(
                        div().max_w(px(screen.0 - 2. * GUTTER)).px(px(GAP)).py(px(10.)).rounded(px(RADIUS)).bg(p.raised).text_color(p.text).text_size(fs(14.)).child(text.clone()),
                    )
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{FIT, MAX_SCALE, View, clamped, fitted, reach, zoomed_about};

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
}
