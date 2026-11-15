//! Hold-to-peek: a photo or video held in the gallery shows whole over the grid while the finger
//! stays down, as in Telegram and iOS. Touch uses GPUI's long press, the same one that lifts a
//! message; a mouse button held for `HOLD` does the same on desktop.

use super::*;

/// GPUI's touch long press is 500 ms too.
const HOLD: Duration = Duration::from_millis(500);
/// How far a held mouse may travel and still peek, as for a message.
const HOLD_SLOP: f32 = 8.;
/// Room left around a peeked picture, inside the system bars.
const MARGIN: f32 = 16.;
const SCRIM: f32 = 0.55;

/// x, y, width, height in window coordinates.
type Rect = (f32, f32, f32, f32);

pub(super) struct Peek {
    id: i64,
    /// The cell it grows out of and shrinks back into.
    cell: Rect,
    /// Width and height as shown, 0 when unknown.
    dims: (f32, f32),
    opened: Instant,
    /// When it began to leave, and how far the scrim was in then.
    closing: Option<(Instant, f32)>,
    /// Where the running motion of the frame began, and when: it heads for the fitted place, or for
    /// the cell once closing, so a release or a late size carries on from where the frame is.
    from: Rect,
    since: Instant,
    /// The frame as last drawn.
    drawn: Rect,
    /// Its full picture asked for already, so a failed read is not repeated.
    tried: bool,
    /// The full picture, and when it was first ready to draw: decoding finishes after the bytes arrive.
    full: Option<(Arc<Image>, Option<Instant>)>,
}

impl Peek {
    /// How far the scrim is in, 0 to 1.
    fn veil(&self, now: Instant, still: bool) -> f32 {
        let (enter, exit) = if still { (motion::ENTER, motion::EXIT) } else { (motion::HERO_IN, motion::HERO_OUT) };
        match self.closing {
            Some((at, from)) => from * (1. - motion::ease_out(frac(now - at, exit))),
            None => motion::ease_out(frac(now - self.opened, enter)),
        }
    }

    fn finished(&self, now: Instant, still: bool) -> bool {
        self.closing.is_some_and(|(at, _)| now - at >= if still { motion::EXIT } else { motion::HERO_OUT })
    }

    /// Starts the frame's motion again from where it is now, toward wherever it now heads.
    fn retarget(&mut self, now: Instant) {
        (self.from, self.since) = (self.drawn, now);
    }
}

pub(super) struct Held {
    id: i64,
    cell: Bounds<Pixels>,
    at: Point<Pixels>,
    since: Instant,
}

fn frac(elapsed: Duration, of: Duration) -> f32 {
    (elapsed.as_secs_f32() / of.as_secs_f32()).min(1.)
}

/// Where a picture of `dims` sits: as large as fits the screen inside the bars and margins,
/// centred, aspect kept; unknown dims make a square. x, y, width, height.
fn fit_rect(view: (f32, f32), top: f32, bottom: f32, dims: (f32, f32)) -> (f32, f32, f32, f32) {
    let (room_w, room_h) = ((view.0 - 2. * MARGIN).max(1.), (view.1 - top - bottom - 2. * MARGIN).max(1.));
    let (w, h) = if dims.0 > 0. && dims.1 > 0. { dims } else { (1., 1.) };
    let scale = (room_w / w).min(room_h / h);
    let (w, h) = (w * scale, h * scale);
    ((view.0 - w) / 2., top + MARGIN + (room_h - h) / 2., w, h)
}

fn lerp(a: Rect, b: Rect, t: f32) -> Rect {
    let mix = |a: f32, b: f32| a + (b - a) * t;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2), mix(a.3, b.3))
}

/// Where the grid's thumbnail, the picture's middle square, falls when the picture of `dims` covers
/// `frame`, so the full picture fading in over it lines up; never wider or taller than the frame.
fn middle_square((x, y, w, h): Rect, dims: (f32, f32)) -> Rect {
    let aspect = if dims.0 > 0. && dims.1 > 0. { dims.0 / dims.1 } else { 1. };
    let (pw, ph) = if w > h * aspect { (w, w / aspect) } else { (h * aspect, h) };
    let side = pw.min(ph).min(w).min(h);
    (x + (w - side) / 2., y + (h - side) / 2., side, side)
}

/// The scale, at most 1, that fits w x h into the screen turned either way, so a decode never
/// holds more pixels than the screen has. Same rule as `Gallery.fit` in Java.
#[cfg_attr(target_os = "android", expect(dead_code, reason = "Java scales on Android"))]
pub(super) fn peek_scale(w: u32, h: u32, screen: (u32, u32)) -> f32 {
    if w == 0 || h == 0 {
        return 1.;
    }
    let (longer, shorter) = (screen.0.max(screen.1) as f32, screen.0.min(screen.1) as f32);
    (longer / w.max(h) as f32).min(shorter / w.min(h) as f32).min(1.)
}

fn dims_of(bytes: &[u8]) -> Option<(f32, f32)> {
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some((w as f32, h as f32))
}

impl BeeperApp {
    fn peek_start(&mut self, id: i64, cell: Bounds<Pixels>, cx: &mut Context<Self>) {
        let Some(item) = self.gallery.items.iter().find(|item| item.id == id) else { return };
        let dims = (item.width as f32, item.height as f32);
        let cell = (f32::from(cell.origin.x), f32::from(cell.origin.y), f32::from(cell.size.width), f32::from(cell.size.height));
        self.drop_peek(cx);
        #[cfg(target_os = "android")]
        motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
        let now = Instant::now();
        self.gallery.peek = Some(Peek { id, cell, dims, opened: now, closing: None, from: cell, since: now, drawn: cell, tried: false, full: None });
        self.pump_peek(cx);
        cx.notify();
    }

    fn peek_end(&mut self, cx: &mut Context<Self>) {
        let (now, still) = (Instant::now(), cx.reduce_motion());
        if let Some(p) = self.gallery.peek.as_mut().filter(|p| p.closing.is_none()) {
            p.closing = Some((now, p.veil(now, still)));
            p.retarget(now);
            cx.notify();
        }
    }

    pub(super) fn drop_peek(&mut self, cx: &mut Context<Self>) {
        if let Some((image, _)) = self.gallery.peek.take().and_then(|p| p.full) {
            image.remove_asset(cx);
        }
    }

    /// Reads the open peek's full picture unless one is being read; the next starts when it is done.
    fn pump_peek(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gallery;
        if g.decoding {
            return;
        }
        let Some(p) = g.peek.as_mut().filter(|p| p.closing.is_none() && !p.tried) else { return };
        let Some(item) = g.items.iter().find(|item| item.id == p.id).cloned() else { return };
        p.tried = true;
        let (opened, screen) = (p.opened, g.screen_px);
        g.decoding = true;
        cx.spawn(async move |this, cx| {
            let bytes = cx.background_spawn(async move { store::full(&item, screen) }).await;
            let _ = this.update(cx, |this, cx| {
                let g = &mut this.gallery;
                g.decoding = false;
                // One that ended or was replaced meanwhile never sees it: the bytes drop here.
                if let Some(p) = g.peek.as_mut().filter(|p| p.opened == opened && p.closing.is_none())
                    && let Some((format, bytes)) = bytes.and_then(|bytes| Some((image_format(&bytes)?, bytes)))
                {
                    // A size first unknown, or known wrong, reshapes the frame in motion rather than at once.
                    if let Some(dims) = dims_of(&bytes).filter(|&dims| dims != p.dims) {
                        p.dims = dims;
                        p.retarget(Instant::now());
                    }
                    p.full = Some((Arc::new(Image::from_bytes(format, bytes)), None));
                    cx.notify();
                }
                this.pump_peek(cx);
            });
        })
        .detach();
    }

    /// A mouse button went down on a photo: a peek unless it moves or lets go first.
    fn hold_start(&mut self, id: i64, cell: Bounds<Pixels>, at: Point<Pixels>, cx: &mut Context<Self>) {
        let since = Instant::now();
        self.gallery.held = Some(Held { id, cell, at, since });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOLD).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(held) = this.gallery.held.take_if(|h| h.since == since) {
                    this.peek_start(held.id, held.cell, cx);
                }
            });
        })
        .detach();
    }

    /// What starts a peek on a cell: a touch's long press, or a held mouse button.
    pub(super) fn peek_probe(&self, id: i64, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                window.on_mouse_event({
                    let (weak, hitbox) = (weak.clone(), hitbox.clone());
                    move |e: &MouseDownEvent, phase, window, cx| {
                        if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                            let _ = weak.update(cx, |this, cx| this.hold_start(id, bounds, e.position, cx));
                        }
                    }
                });
                // Claiming the press keeps the grid from scrolling and the lift from tapping.
                window.on_mouse_event(move |e: &LongPressEvent, phase, window, cx| {
                    if !phase.bubble() || e.phase != TouchPhase::Started || window.default_prevented() || !hitbox.is_hovered(window) {
                        return;
                    }
                    let Some(app) = weak.upgrade() else { return };
                    window.prevent_default();
                    window.capture_long_press(&app);
                    cx.stop_propagation();
                    app.update(cx, |this, cx| this.peek_start(id, bounds, cx));
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// Window-wide listeners while the sheet is open: the press that peeks may end anywhere.
    pub(super) fn peek_tracker(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event({
                    let weak = weak.clone();
                    move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase.bubble() {
                            let _ = weak.update(cx, |this, _| {
                                let at = e.position;
                                this.gallery.held.take_if(|h| f32::from(at.x - h.at.x).hypot(f32::from(at.y - h.at.y)) > HOLD_SLOP);
                            });
                        }
                    }
                });
                window.on_mouse_event({
                    let weak = weak.clone();
                    move |e: &MouseUpEvent, phase, _, cx| {
                        if phase.bubble() && e.button == MouseButton::Left {
                            let _ = weak.update(cx, |this, cx| {
                                this.gallery.held = None;
                                this.peek_end(cx);
                            });
                        }
                    }
                });
                window.on_mouse_event(move |e: &LongPressEvent, phase, _, cx| {
                    if phase.bubble() && matches!(e.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                        let _ = weak.update(cx, |this, cx| this.peek_end(cx));
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// The peeked item over the sheet. It grows out of its cell into place, the cell's thumbnail
    /// shown where the picture's middle square falls, and the full picture fades in over it at the
    /// same geometry once decoded; let go, it shrinks back into the cell. It blocks the pointer, so a
    /// held mouse's release taps nothing.
    pub(super) fn render_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let now = Instant::now();
        let still = cx.reduce_motion();
        if self.gallery.peek.as_ref().is_some_and(|p| p.finished(now, still)) {
            self.drop_peek(cx);
        }
        let p = Palette::of(cx);
        let view = frame::viewport(window);
        let (top, bottom) = crate::insets();
        let g = &mut self.gallery;
        let peek = g.peek.as_mut()?;
        let (id, veil, leaving) = (peek.id, peek.veil(now, still), peek.closing.is_some());
        let fitted = fit_rect((f32::from(view.width), f32::from(view.height)), top, bottom, peek.dims);
        let (target, span) = if leaving { (peek.cell, motion::HERO_OUT) } else { (fitted, motion::HERO_IN) };
        let moving = !still && now - peek.since < span;
        let frame = if still { fitted } else { lerp(peek.from, target, motion::ease_out(frac(now - peek.since, span))) };
        peek.drawn = frame;
        let full = peek.full.as_mut().and_then(|(image, ready)| {
            if ready.is_none() && image.clone().use_render_image(window, cx).is_some() {
                *ready = Some(now);
            }
            Some((image.clone(), (*ready)?))
        });
        let sharp = full.as_ref().map_or(0., |(_, at)| motion::ease_out(frac(now - *at, motion::SHARPEN)));
        let fading = if still { now - peek.opened < motion::ENTER } else { now - peek.opened < motion::HERO_IN };
        if moving || fading || leaving || full.is_some() && sharp < 1. {
            window.request_animation_frame();
        }
        let (x, y, w, h) = frame;
        let (sx, sy, side, _) = middle_square(frame, peek.dims);
        let radius = if still { RADIUS } else { RADIUS * veil };
        // Its corners are the frame's only where it reaches them.
        let inner = if (w - side) / 2. < radius && (h - side) / 2. < radius { radius } else { 0. };
        let video = g.items.iter().find(|item| item.id == id).filter(|item| item.video()).map(|item| item.duration_ms);
        let thumb = (sharp < 1.).then(|| g.thumbs.get(id)).flatten();
        Some(
            div()
                .id("gallery-peek")
                .absolute()
                .inset_0()
                // Leaving, it lets the next tap through to the grid.
                .when(!leaving, |el| el.occlude())
                .bg(gpui_kit::black().opacity(SCRIM * veil))
                .child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(w))
                        .h(px(h))
                        .rounded(px(radius))
                        .bg(p.raised)
                        .when(still, |el| el.opacity(veil))
                        // An `img` with an auto size takes its pixels' size, so each gets an explicit one.
                        .children(thumb.map(|image| {
                            img(image).absolute().left(px(sx - x)).top(px(sy - y)).size(px(side)).rounded(px(inner)).object_fit(ObjectFit::Cover)
                        }))
                        .children(full.map(|(image, _)| img(image).absolute().inset_0().size_full().rounded(px(radius)).object_fit(ObjectFit::Cover).opacity(sharp)))
                        .children(video.map(|ms| {
                            div()
                                .absolute()
                                .left(px(12.))
                                .bottom(px(12.))
                                .px_2()
                                .rounded_full()
                                .bg(gpui_kit::black().opacity(0.55))
                                .text_color(gpui_kit::white())
                                .text_size(fs(13.))
                                .font_features(tabular())
                                .opacity(veil)
                                .child(length(Duration::from_millis(ms)))
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{MARGIN, fit_rect, lerp, middle_square, peek_scale};

    #[test]
    fn a_peek_fits_inside_the_bars_with_its_aspect() {
        // A 4:3 landscape photo on a 400 x 800 phone with 40 px of bars above and 20 below.
        let (x, y, w, h) = fit_rect((400., 800.), 40., 20., (4000., 3000.));
        assert_eq!((x, w), (MARGIN, 400. - 2. * MARGIN));
        assert!((h - w * 0.75).abs() < 0.01);
        assert!((y - (40. + MARGIN + (800. - 60. - 2. * MARGIN - h) / 2.)).abs() < 0.01);
        // A tall one is limited by the height instead, and unknown dims make a square.
        let (_, y, w, h) = fit_rect((400., 800.), 40., 20., (1000., 4000.));
        assert_eq!((y, h), (40. + MARGIN, 800. - 60. - 2. * MARGIN));
        assert!((w - h / 4.).abs() < 0.01);
        let (_, _, w, h) = fit_rect((400., 800.), 0., 0., (0., 0.));
        assert_eq!(w, h);
    }

    #[test]
    fn the_thumbnail_stays_where_the_full_picture_puts_its_middle() {
        // A 4:3 photo growing from a 100 px cell to 368 x 276: the thumbnail is the middle square,
        // so it fills the cell at the start, the frame's height at the end, and in between too.
        let (cell, end) = ((10., 500., 100., 100.), (16., 200., 368., 276.));
        assert_eq!(middle_square(cell, (4000., 3000.)), cell);
        assert_eq!(middle_square(end, (4000., 3000.)), (16. + 46., 200., 276., 276.));
        let mid = lerp(cell, end, 0.5);
        let (x, y, side, _) = middle_square(mid, (4000., 3000.));
        assert_eq!((y, side), (mid.1, mid.3));
        assert!((x + side / 2. - (mid.0 + mid.2 / 2.)).abs() < 0.01);
        // A tall one keeps the frame's width; an unknown size draws it whole.
        assert_eq!(middle_square((0., 0., 300., 600.), (1000., 2000.)), (0., 150., 300., 300.));
        assert_eq!(middle_square(cell, (0., 0.)), cell);
        // Never larger than the frame, whatever the guess.
        let (_, _, side, _) = middle_square((0., 0., 300., 100.), (1000., 1000.));
        assert_eq!(side, 100.);
    }

    #[test]
    fn a_decode_never_holds_more_than_a_screenful() {
        let screen = (1080, 2340);
        for (w, h) in [(4000, 3000), (3000, 4000), (4000, 4000), (12000, 500), (800, 600)] {
            let s = peek_scale(w, h, screen);
            assert!(s <= 1.);
            assert!((w as f32 * s) * (h as f32 * s) <= 1080. * 2340. + 1., "{w}x{h}");
        }
        assert_eq!(peek_scale(800, 600, screen), 1.);
        assert_eq!(peek_scale(0, 0, screen), 1.);
    }
}
