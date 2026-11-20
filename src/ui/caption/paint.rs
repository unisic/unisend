//! The photo editor's model and its raster, apart from GPUI so both can be tested: the frame
//! (quarter turns, then a crop), pen and blur strokes, and text labels. Points live in the upright
//! original picture, from 0 to 1 on each side, so a turn or a crop made later keeps them on the
//! spot they were drawn on; widths and text sizes are fractions of the original's long side.
//!
//! The same `render` draws the editor's 1280 px picture after each change and the full picture
//! sent at Done, so what is sent is what was shown.

use image::{RgbImage, imageops, imageops::FilterType};

/// Longest side of an edited picture: 4096 x 3072 is 38 MB as RGB while it is drawn.
pub const CANVAS_PX: u32 = 4096;
/// Strokes and labels together; each undo step takes one back.
pub const MAX_LAYERS: usize = 100;
/// Points kept per stroke; a longer stroke ends there.
pub const MAX_POINTS: usize = 1000;
/// Characters per label.
pub const MAX_TEXT: usize = 120;
const MAX_HISTORY: usize = 200;
/// The smallest crop, as a share of the turned picture's side.
pub const MIN_CROP: f32 = 0.1;
/// A blur's cell, as a share of the long side: Signal's blur leaves no feature readable.
const BLUR_CELL: f32 = 1. / 48.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Quarter turns clockwise, 0 to 3.
    pub turns: u8,
    /// `[x0, y0, x1, y1]` of the turned picture, 0 to 1.
    pub crop: [f32; 4],
}

impl Default for Frame {
    fn default() -> Self {
        Self { turns: 0, crop: [0., 0., 1., 1.] }
    }
}

/// One quarter turn clockwise of a point in a unit square.
fn turn([u, v]: [f32; 2]) -> [f32; 2] {
    [1. - v, u]
}

fn unturn([x, y]: [f32; 2]) -> [f32; 2] {
    [y, 1. - x]
}

fn span(a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
}

impl Frame {
    /// A point of the original as a share of the framed picture (outside 0..1 when cropped away).
    pub fn view_of(&self, mut p: [f32; 2]) -> [f32; 2] {
        for _ in 0..self.turns {
            p = turn(p);
        }
        let [x0, y0, x1, y1] = self.crop;
        [(p[0] - x0) / (x1 - x0), (p[1] - y0) / (y1 - y0)]
    }

    pub fn original_of(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let [x0, y0, x1, y1] = self.crop;
        let mut p = [x0 + x * (x1 - x0), y0 + y * (y1 - y0)];
        for _ in 0..self.turns {
            p = unturn(p);
        }
        p
    }

    /// A quarter turn clockwise; the crop turns with the picture.
    pub fn rotate(&mut self) {
        let [x0, y0, x1, y1] = self.crop;
        self.crop = span(turn([x0, y0]), turn([x1, y1]));
        self.turns = (self.turns + 1) % 4;
    }

    /// The crop as a rectangle of the original.
    fn crop_in_original(&self) -> [f32; 4] {
        let [x0, y0, x1, y1] = self.crop;
        let (mut a, mut b) = ([x0, y0], [x1, y1]);
        for _ in 0..self.turns {
            (a, b) = (unturn(a), unturn(b));
        }
        span(a, b)
    }

    /// Pixels of the framed picture per unit of the original's long side, for a framed picture
    /// `w` x `h` px.
    pub fn unit(&self, w: f32, h: f32) -> f32 {
        let [x0, y0, x1, y1] = self.crop;
        (w / (x1 - x0)).max(h / (y1 - y0))
    }
}

/// A crop rectangle kept inside the picture and no smaller than `MIN_CROP` on a side.
pub fn clamp_crop([x0, y0, x1, y1]: [f32; 4]) -> [f32; 4] {
    let x0 = x0.clamp(0., 1. - MIN_CROP);
    let y0 = y0.clamp(0., 1. - MIN_CROP);
    [x0, y0, x1.clamp(x0 + MIN_CROP, 1.), y1.clamp(y0 + MIN_CROP, 1.)]
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Pen([u8; 3]),
    Blur,
}

#[derive(Clone, Debug)]
pub struct Stroke {
    pub ink: Ink,
    /// Share of the original's long side.
    pub width: f32,
    pub points: Vec<[f32; 2]>,
}

impl Stroke {
    /// Adds a point unless it is closer than a fifth of the width to the last; false when full.
    pub fn extend(&mut self, p: [f32; 2]) -> bool {
        if self.points.len() >= MAX_POINTS {
            return false;
        }
        let near = self.points.last().is_some_and(|q| (q[0] - p[0]).hypot(q[1] - p[1]) < self.width / 5.);
        if !near {
            self.points.push(p);
        }
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    /// Its centre in the original.
    pub at: [f32; 2],
    /// Font size as a share of the original's long side.
    pub size: f32,
    pub color: [u8; 3],
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Stroke,
    Label,
    Frame(Frame),
    Moved(usize, [f32; 2]),
}

/// Everything done to one picture, with the way back.
#[derive(Clone, Debug, Default)]
pub struct Edits {
    pub frame: Frame,
    pub strokes: Vec<Stroke>,
    pub labels: Vec<Label>,
    history: Vec<Step>,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        self.frame == Frame::default() && self.strokes.is_empty() && self.labels.is_empty()
    }

    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn full(&self) -> bool {
        self.strokes.len() + self.labels.len() >= MAX_LAYERS
    }

    fn record(&mut self, step: Step) {
        if self.history.len() >= MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(step);
    }

    pub fn add_stroke(&mut self, stroke: Stroke) -> bool {
        if self.full() || stroke.points.is_empty() {
            return false;
        }
        self.strokes.push(stroke);
        self.record(Step::Stroke);
        true
    }

    pub fn add_label(&mut self, mut label: Label) -> bool {
        label.text = label.text.trim().chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect();
        if self.full() || label.text.is_empty() {
            return false;
        }
        self.labels.push(label);
        self.record(Step::Label);
        true
    }

    /// Records the frame as it was before a change the caller made.
    pub fn framed(&mut self, before: Frame) {
        if before != self.frame {
            self.record(Step::Frame(before));
        }
    }

    /// Records where a label was before the caller moved it.
    pub fn moved(&mut self, ix: usize, from: [f32; 2]) {
        if self.labels.get(ix).is_some_and(|l| l.at != from) {
            self.record(Step::Moved(ix, from));
        }
    }

    pub fn undo(&mut self) {
        match self.history.pop() {
            Some(Step::Stroke) => drop(self.strokes.pop()),
            Some(Step::Label) => drop(self.labels.pop()),
            Some(Step::Frame(frame)) => self.frame = frame,
            Some(Step::Moved(ix, at)) => {
                if let Some(label) = self.labels.get_mut(ix) {
                    label.at = at;
                }
            }
            None => {}
        }
    }
}

/// Draws a label into the picture at its centre in px, at its font size in px.
pub type DrawLabel<'a> = dyn FnMut(&mut RgbImage, &Label, [f32; 2], f32) + 'a;

/// `base`, the upright original at any scale, framed and drawn on: at most `max` px on the long
/// side once cropped. Labels are left to `labels`, called last with the picture, each label, its
/// centre in px and its font size in px.
pub fn render(base: RgbImage, edits: &Edits, max: u32, labels: &mut DrawLabel) -> RgbImage {
    let (w, h) = (base.width() as f32, base.height() as f32);
    let [x0, y0, x1, y1] = edits.frame.crop_in_original();
    let (cx, cy) = ((x0 * w).round() as u32, (y0 * h).round() as u32);
    let cw = ((x1 * w).round() as u32).saturating_sub(cx).max(1);
    let ch = ((y1 * h).round() as u32).saturating_sub(cy).max(1);
    let mut picture = if (cx, cy, cw, ch) == (0, 0, base.width(), base.height()) {
        base
    } else {
        let part = imageops::crop_imm(&base, cx, cy, cw, ch).to_image();
        drop(base);
        part
    };
    if picture.width().max(picture.height()) > max {
        let (fw, fh) = fit_size(picture.width(), picture.height(), max);
        picture = imageops::resize(&picture, fw, fh, FilterType::Lanczos3);
    }
    picture = match edits.frame.turns {
        1 => imageops::rotate90(&picture),
        2 => imageops::rotate180(&picture),
        3 => imageops::rotate270(&picture),
        _ => picture,
    };
    let (ow, oh) = (picture.width() as f32, picture.height() as f32);
    let unit = edits.frame.unit(ow, oh);
    let place = |p: [f32; 2]| {
        let [x, y] = edits.frame.view_of(p);
        [x * ow, y * oh]
    };
    for stroke in &edits.strokes {
        let points: Vec<[f32; 2]> = stroke.points.iter().map(|&p| place(p)).collect();
        let Some(mask) = Mask::of(&points, stroke.width * unit / 2., picture.width(), picture.height()) else { continue };
        match stroke.ink {
            Ink::Pen(color) => mask.paint(&mut picture, |_, _| color),
            Ink::Blur => {
                let blurred = blur(&picture, &mask, (unit * BLUR_CELL).max(2.) as u32);
                mask.paint(&mut picture, |x, y| blurred.get_pixel(x - mask.x, y - mask.y).0);
            }
        }
    }
    for label in &edits.labels {
        labels(&mut picture, label, place(label.at), label_px(label.size, unit, ow, oh));
    }
    picture
}

/// A label's font size in px on a framed picture `w` x `h` with `unit`; no larger than half the
/// picture, which also bounds a glyph's raster.
pub fn label_px(size: f32, unit: f32, w: f32, h: f32) -> f32 {
    (size * unit).min(w.max(h) / 2.)
}

/// `w` x `h` scaled to at most `max` on the long side.
fn fit_size(w: u32, h: u32, max: u32) -> (u32, u32) {
    let scale = max as f32 / w.max(h) as f32;
    (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1))
}

/// A stroke's coverage over the box it touches, anti-aliased, 0 to 255.
struct Mask {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    cover: Vec<u8>,
}

impl Mask {
    fn of(points: &[[f32; 2]], radius: f32, width: u32, height: u32) -> Option<Self> {
        let radius = radius.max(0.5);
        let reach = radius + 1.;
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in points {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        let x = (lo[0] - reach).floor().clamp(0., width as f32) as u32;
        let y = (lo[1] - reach).floor().clamp(0., height as f32) as u32;
        let x_end = (hi[0] + reach).ceil().clamp(0., width as f32) as u32;
        let y_end = (hi[1] + reach).ceil().clamp(0., height as f32) as u32;
        if x_end <= x || y_end <= y {
            return None;
        }
        let (w, h) = (x_end - x, y_end - y);
        let mut cover = vec![0u8; (w * h) as usize];
        let pairs = points.windows(2).map(|s| (s[0], s[1])).chain((points.len() == 1).then(|| (points[0], points[0])));
        for (a, b) in pairs {
            let sx = ((a[0].min(b[0]) - reach).floor().max(x as f32) as u32).min(x_end);
            let sy = ((a[1].min(b[1]) - reach).floor().max(y as f32) as u32).min(y_end);
            let ex = ((a[0].max(b[0]) + reach).ceil().max(0.) as u32).min(x_end);
            let ey = ((a[1].max(b[1]) + reach).ceil().max(0.) as u32).min(y_end);
            for py in sy..ey {
                for px in sx..ex {
                    let d = distance([px as f32 + 0.5, py as f32 + 0.5], a, b);
                    let c = ((radius + 0.5 - d).clamp(0., 1.) * 255.) as u8;
                    let cell = &mut cover[((py - y) * w + (px - x)) as usize];
                    *cell = (*cell).max(c);
                }
            }
        }
        Some(Self { x, y, w, h, cover })
    }

    /// Blends `color(x, y)` over the picture where the stroke covers it.
    fn paint(&self, picture: &mut RgbImage, color: impl Fn(u32, u32) -> [u8; 3]) {
        for dy in 0..self.h {
            for dx in 0..self.w {
                let c = self.cover[(dy * self.w + dx) as usize];
                if c == 0 {
                    continue;
                }
                let (x, y) = (self.x + dx, self.y + dy);
                let ink = color(x, y);
                let px = picture.get_pixel_mut(x, y);
                for (dst, src) in px.0.iter_mut().zip(ink) {
                    *dst = blend(*dst, src, c);
                }
            }
        }
    }
}

pub fn blend(dst: u8, src: u8, alpha: u8) -> u8 {
    ((src as u32 * alpha as u32 + dst as u32 * (255 - alpha as u32) + 127) / 255) as u8
}

fn distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (ab, ap) = ([b[0] - a[0], b[1] - a[1]], [p[0] - a[0], p[1] - a[1]]);
    let len = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len > 0. { ((ap[0] * ab[0] + ap[1] * ab[1]) / len).clamp(0., 1.) } else { 0. };
    (ap[0] - t * ab[0]).hypot(ap[1] - t * ab[1])
}

/// The mask's box of `picture`, averaged into cells of `cell` px and smoothed back up.
fn blur(picture: &RgbImage, mask: &Mask, cell: u32) -> RgbImage {
    let part = imageops::crop_imm(picture, mask.x, mask.y, mask.w, mask.h).to_image();
    let small = imageops::resize(&part, mask.w.div_ceil(cell).max(1), mask.h.div_ceil(cell).max(1), FilterType::Triangle);
    imageops::resize(&small, mask.w, mask.h, FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5
    }

    #[test]
    fn a_point_comes_back_through_any_frame() {
        let mut frame = Frame { turns: 0, crop: [0.2, 0.1, 0.9, 0.6] };
        for _ in 0..5 {
            for p in [[0.3, 0.4], [0.5, 0.2], [0.85, 0.55]] {
                assert!(close(frame.original_of(frame.view_of(p)), p), "{frame:?}");
            }
            frame.rotate();
        }
    }

    #[test]
    fn a_turn_keeps_the_crop_on_the_same_part_of_the_picture() {
        let mut frame = Frame { turns: 0, crop: [0.5, 0., 1., 0.5] };
        let before = frame.crop_in_original();
        frame.rotate();
        assert_eq!(frame.turns, 1);
        assert_eq!(frame.crop, [0.5, 0.5, 1., 1.]);
        assert_eq!(frame.crop_in_original(), before);
        // The top right corner of the original is the bottom right one after a turn clockwise.
        assert!(close(frame.view_of([1., 0.]), [1., 1.]));
        for _ in 0..3 {
            frame.rotate();
        }
        assert_eq!(frame, Frame { turns: 0, crop: [0.5, 0., 1., 0.5] });
    }

    #[test]
    fn a_crop_stays_inside_and_not_too_small() {
        assert_eq!(clamp_crop([-0.2, 0.3, 1.4, 0.32]), [0., 0.3, 1., 0.3 + MIN_CROP]);
        assert_eq!(clamp_crop([0.95, 0., 0.97, 1.]), [1. - MIN_CROP, 0., 1., 1.]);
    }

    #[test]
    fn undo_takes_back_each_change_in_turn() {
        let mut edits = Edits::default();
        assert!(edits.add_stroke(Stroke { ink: Ink::Pen([255, 0, 0]), width: 0.01, points: vec![[0.1, 0.1]] }));
        let before = edits.frame;
        edits.frame.rotate();
        edits.framed(before);
        assert!(edits.add_label(Label { text: "  hi\n ".into(), at: [0.5, 0.5], size: 0.05, color: [255; 3] }));
        assert_eq!(edits.labels[0].text, "hi");
        edits.labels[0].at = [0.2, 0.2];
        edits.moved(0, [0.5, 0.5]);
        assert!(!edits.add_label(Label { text: " ".into(), at: [0.5, 0.5], size: 0.05, color: [255; 3] }));
        edits.undo();
        assert_eq!(edits.labels[0].at, [0.5, 0.5]);
        edits.undo();
        assert!(edits.labels.is_empty());
        edits.undo();
        assert_eq!(edits.frame, Frame::default());
        edits.undo();
        assert!(edits.is_empty() && !edits.can_undo());
    }

    #[test]
    fn layers_and_points_are_bounded() {
        let mut edits = Edits::default();
        let dot = Stroke { ink: Ink::Blur, width: 0.01, points: vec![[0.5, 0.5]] };
        while edits.add_stroke(dot.clone()) {}
        assert_eq!(edits.strokes.len(), MAX_LAYERS);
        let mut stroke = Stroke { ink: Ink::Blur, width: 0.001, points: Vec::new() };
        let mut n = 0;
        while stroke.extend([n as f32 / 4000., 0.]) {
            n += 1;
        }
        assert_eq!(stroke.points.len(), MAX_POINTS);
        let label = Label { text: "x".repeat(500), at: [0.5, 0.5], size: 0.05, color: [0; 3] };
        let mut edits = Edits::default();
        edits.add_label(label);
        assert_eq!(edits.labels[0].text.chars().count(), MAX_TEXT);
    }

    fn checker(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| if (x / 2 + y / 2) % 2 == 0 { image::Rgb([0, 0, 0]) } else { image::Rgb([255, 255, 255]) })
    }

    #[test]
    fn render_crops_turns_and_bounds_the_picture() {
        // 400 x 200, its left half red: cropped to the left half and turned, it is 200 x 200 red.
        let base = RgbImage::from_fn(400, 200, |x, _| if x < 200 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut edits = Edits::default();
        edits.frame.crop = [0., 0., 0.5, 1.];
        edits.frame.rotate();
        let out = render(base.clone(), &edits, CANVAS_PX, &mut |_, _, _, _| {});
        assert_eq!(out.dimensions(), (200, 200));
        assert!(out.pixels().all(|p| p.0 == [255, 0, 0]));
        let out = render(base, &Edits::default(), 100, &mut |_, _, _, _| {});
        assert_eq!(out.dimensions(), (100, 50));
    }

    #[test]
    fn a_pen_stroke_lands_where_it_was_drawn_after_a_turn() {
        let mut edits = Edits::default();
        // A dot at the original's top left corner area.
        edits.add_stroke(Stroke { ink: Ink::Pen([255, 0, 0]), width: 0.05, points: vec![[0.1, 0.1]] });
        edits.frame.rotate();
        let out = render(RgbImage::new(200, 100), &edits, CANVAS_PX, &mut |_, _, _, _| {});
        assert_eq!(out.dimensions(), (100, 200));
        // Turned clockwise, the top left corner goes to the top right.
        assert_eq!(out.get_pixel(90, 20).0, [255, 0, 0]);
        assert_eq!(out.get_pixel(10, 20).0, [0, 0, 0]);
        assert_eq!(out.get_pixel(50, 150).0, [0, 0, 0]);
    }

    #[test]
    fn blur_wipes_out_detail_under_the_stroke_only() {
        let mut edits = Edits::default();
        edits.add_stroke(Stroke { ink: Ink::Blur, width: 0.3, points: vec![[0.25, 0.5], [0.3, 0.5]] });
        let out = render(checker(200, 200), &edits, CANVAS_PX, &mut |_, _, _, _| {});
        let grey = |p: &image::Rgb<u8>| p.0.iter().all(|&c| (90..=165).contains(&c));
        assert!(grey(out.get_pixel(55, 100)), "{:?}", out.get_pixel(55, 100));
        assert_eq!(out.get_pixel(190, 190), checker(200, 200).get_pixel(190, 190));
    }

    #[test]
    fn labels_are_placed_in_the_framed_picture() {
        let mut edits = Edits::default();
        edits.add_label(Label { text: "a".into(), at: [0.75, 0.5], size: 0.1, color: [0; 3] });
        edits.frame.crop = [0.5, 0., 1., 1.];
        let mut seen = Vec::new();
        render(RgbImage::new(400, 200), &edits, CANVAS_PX, &mut |_, label, at, size| seen.push((label.text.clone(), at, size)));
        assert_eq!(seen, vec![("a".to_owned(), [100., 100.], 40.)]);
    }
}
