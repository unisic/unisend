//! The photo editor Signal has before a picture leaves: crop and quarter turns, drawing in a few
//! colours and widths, text, a blur brush, and undo. It opens from a photo's pencil in the
//! preview or in the desktop's tray. Done writes the edited picture as a new JPEG (q85, no
//! metadata, at most `paint::CANVAS_PX` on the long side) in the account's `pasted` folder, which
//! the send deletes, and the item sends it in place of the original; HD then decides its size
//! as for any photo. The edits stay with the item, so the editor opens again where it was left.
//!
//! "Blur faces" is on Android only: it asks `android.media.FaceDetector` (on the device, as Signal
//! does), which finds faces turned towards the camera, and blurs a disc over each. Desktop has no
//! face detector without a new crate, so there the brush is the only way; it also covers what
//! the detector misses.
//!
//! Memory: the original at 1280 px is kept as RGB (5 MB at 4:3) and drawn on a background thread
//! after each change, one drawing at a time (a change meanwhile waits for it), into a BMP shown
//! as an image (5 MB, and GPUI's decoded copy). Done decodes the whole original under
//! `full_picture`: about twice its RGB size at the peak (36 MB per copy for 12 MP). Points are
//! bounded by `paint::MAX_LAYERS` x `paint::MAX_POINTS` (800 KB); a label's glyphs are drawn
//! into the picture one at a time. "Blur faces" hands the detector the 1280 px picture as RGB 565
//! (2.5 MB in Rust, a Java copy and a bitmap of it) and takes back at most 20 faces.

use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

use image::{DynamicImage, RgbImage, imageops::FilterType};

use super::paint::{self, Edits, Frame, Ink, Label, Stroke};
use super::*;

/// Signal's pen colours, roughly: white, black and six hues.
const COLORS: [[u8; 3]; 8] = [[255, 255, 255], [0, 0, 0], [229, 72, 77], [245, 140, 35], [250, 204, 21], [52, 199, 89], [46, 110, 240], [175, 82, 222]];
/// Widths as shares of the long side, thin to thick.
const PEN: [f32; 3] = [0.006, 0.012, 0.024];
const BLUR: [f32; 3] = [0.03, 0.06, 0.1];
const TEXT: [f32; 3] = [0.04, 0.07, 0.11];
/// A crop corner is taken this close to the finger (px).
const GRAB: f32 = 36.;
/// Labels are shaped at this size and scaled to theirs.
const SHAPED_PX: f32 = 100.;

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    /// None chosen: labels can be moved.
    Move,
    Crop,
    Pen,
    Text,
    Blur,
}

enum Drag {
    Stroke(Stroke),
    /// A crop corner (0 top left, 1 top right, 2 bottom left, 3 bottom right) and the frame before.
    Corner(usize, Frame),
    /// The whole crop, from where the finger went down.
    Crop([f32; 2], Frame),
    /// A label, where it was, and the finger's offset from its centre.
    Label(usize, [f32; 2], [f32; 2]),
}

pub(super) struct Editor {
    /// The preview entry it edits.
    entry: u64,
    /// Tells this opening's drawings from an earlier one's.
    serial: u64,
    source: PathBuf,
    edits: Edits,
    tool: Tool,
    color: usize,
    size: usize,
    base: Option<Arc<RgbImage>>,
    /// The drawn picture and its size in px.
    shown: Option<(Arc<Image>, (f32, f32))>,
    drawing: bool,
    /// Changed while a drawing ran.
    stale: bool,
    drag: Option<Drag>,
    typing: bool,
    saving: bool,
    /// Looking for faces.
    finding: bool,
    field: Entity<TextareaState>,
    /// Where the picture was last painted.
    rect: Rc<Cell<Bounds<Pixels>>>,
    _subs: Vec<Subscription>,
}

impl Editor {
    /// The frame the picture is shown in: whole, only turned, while cropping.
    fn view_frame(&self) -> Frame {
        if self.tool == Tool::Crop { Frame { turns: self.edits.frame.turns, crop: [0., 0., 1., 1.] } } else { self.edits.frame }
    }
}

/// A picture the editor can take: a still the send would re-encode anyway.
pub(super) fn editable(path: &Path) -> bool {
    prepare::recodable(&mime_guess::from_path(path).first_or_octet_stream())
}

fn hsla([r, g, b]: [u8; 3]) -> Hsla {
    Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: 1. }.into()
}

fn label_font() -> Font {
    Font { weight: FontWeight::SEMIBOLD, ..font(crate::FACE) }
}

fn contain(bounds: Bounds<Pixels>, (w, h): (f32, f32)) -> Bounds<Pixels> {
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let k = (bw / w).min(bh / h);
    let (dw, dh) = (w * k, h * k);
    Bounds::new(point(bounds.origin.x + px((bw - dw) / 2.), bounds.origin.y + px((bh - dh) / 2.)), size(px(dw), px(dh)))
}

/// A label's glyphs, shaped at `SHAPED_PX`.
struct Shaped {
    glyphs: Vec<(FontId, GlyphId, f32, bool)>,
    width: f32,
    ascent: f32,
    descent: f32,
}

fn shape(label: &Label, window: &Window) -> Shaped {
    let run = TextRun { len: label.text.len(), font: label_font(), color: hsla(label.color), background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(label.text.clone().into(), px(SHAPED_PX), &[run], None);
    let glyphs = line.runs.iter().flat_map(|run| run.glyphs.iter().map(|g| (run.font_id, g.id, f32::from(g.position.x), g.is_emoji))).collect();
    Shaped { glyphs, width: f32::from(line.width), ascent: f32::from(line.ascent), descent: f32::from(line.descent) }
}

/// Draws a label's glyphs into `picture`, centred on `at`, one glyph raster at a time. The box
/// of ascent and descent is centred, as a line of that height lays it out on screen.
fn draw_label(text_system: &TextSystem, picture: &mut RgbImage, shaped: &Shaped, color: [u8; 3], at: [f32; 2], size: f32) {
    let k = size / SHAPED_PX;
    let left = at[0] - shaped.width * k / 2.;
    let baseline = (at[1] - (shaped.ascent + shaped.descent) * k / 2. + shaped.ascent * k).round() as i64;
    let (pw, ph) = (picture.width() as i64, picture.height() as i64);
    for &(font_id, glyph_id, x, is_emoji) in &shaped.glyphs {
        let params = RenderGlyphParams {
            font_id,
            glyph_id,
            font_size: px(size),
            subpixel_variant: Point::default(),
            scale_factor: 1.,
            is_emoji,
            subpixel_rendering: false,
            dilation: 0,
        };
        let Ok(bounds) = text_system.raster_bounds(&params) else { continue };
        if bounds.size.width.0 <= 0 || bounds.size.height.0 <= 0 {
            continue;
        }
        let Ok((raster, bytes)) = text_system.rasterize_glyph(&params) else { continue };
        let (w, h) = (raster.width.0.max(0) as usize, raster.height.0.max(0) as usize);
        let colored = bytes.len() == w * h * 4;
        if !colored && bytes.len() != w * h {
            continue;
        }
        let ox = (left + x * k).round() as i64 + bounds.origin.x.0 as i64;
        let oy = baseline + bounds.origin.y.0 as i64;
        for gy in 0..h {
            for gx in 0..w {
                let (x, y) = (ox + gx as i64, oy + gy as i64);
                if x < 0 || y < 0 || x >= pw || y >= ph {
                    continue;
                }
                let (ink, alpha) = if colored {
                    let b = &bytes[(gy * w + gx) * 4..][..4];
                    ([b[2], b[1], b[0]], b[3])
                } else {
                    (color, bytes[gy * w + gx])
                };
                if alpha == 0 {
                    continue;
                }
                let pixel = picture.get_pixel_mut(x as u32, y as u32);
                for (dst, src) in pixel.0.iter_mut().zip(ink) {
                    *dst = paint::blend(*dst, src, alpha);
                }
            }
        }
    }
}

/// The whole original with the edits, written as `image.jpg` in `folder`.
fn save(source: &Path, edits: &Edits, shaped: &[Shaped], text_system: &TextSystem, folder: &Path) -> anyhow::Result<PathBuf> {
    let bytes = {
        let _one = prepare::full_picture();
        let base = prepare::open(source)?.into_rgb8();
        let mut shaped = shaped.iter();
        let picture = paint::render(base, edits, paint::CANVAS_PX, &mut |picture, label, at, size| {
            if let Some(glyphs) = shaped.next() {
                draw_label(text_system, picture, glyphs, label.color, at, size);
            }
        });
        prepare::jpeg(&picture)?
    };
    std::fs::create_dir_all(folder)?;
    let path = folder.join("image.jpg");
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// The original at the preview's size, upright.
fn load_base(source: &Path) -> anyhow::Result<RgbImage> {
    let _one = prepare::full_picture();
    let picture = prepare::open(source)?;
    let max = prepare::PREVIEW_PX;
    let picture = if picture.width().max(picture.height()) > max { picture.resize(max, max, FilterType::Triangle) } else { picture };
    Ok(picture.into_rgb8())
}

fn bmp(picture: RgbImage) -> anyhow::Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(picture).write_to(&mut out, image::ImageFormat::Bmp)?;
    Ok(out.into_inner())
}

/// Faces as Android's detector reports them (the eyes' midpoint and the distance between the eyes,
/// in px of a `w` x `h` picture) turned into blur strokes: a disc from brow to chin, centred a
/// little below the eyes as Signal places its face box.
#[cfg(any(target_os = "android", test))]
fn face_strokes(found: &[f32], w: f32, h: f32) -> Vec<Stroke> {
    found
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|f| f[2] > 0.)
        .map(|f| Stroke { ink: Ink::Blur, width: 3.2 * f[2] / w.max(h), points: vec![[f[0] / w, (f[1] + 0.4 * f[2]) / h]] })
        .collect()
}

/// The faces in `base` as blur strokes, found on the device; the picture leaves Rust only for
/// Android's own detector.
#[cfg(target_os = "android")]
fn find_faces(base: &RgbImage) -> anyhow::Result<Vec<Stroke>> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JFloatArray, JValue};

    // The detector wants RGB 565 at an even width.
    let (w, h) = (base.width() & !1, base.height());
    let mut pixels = Vec::with_capacity((w * h * 2) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = base.get_pixel(x, y).0.map(u16::from);
            pixels.extend(((r >> 3) << 11 | (g >> 2) << 5 | b >> 3).to_le_bytes());
        }
    }
    let found = helpers::with_env(|env| {
        let class = helpers::find_app_class(env, "dev.gpui.beeper.Faces")?;
        let array = env.byte_array_from_slice(&pixels).map_err(|e| e.to_string())?;
        let size = |n: u32| JValue::Int(n as i32);
        let found = env
            .call_static_method(&class, jni::jni_str!("find"), jni::jni_sig!("([BII)[F"), &[JValue::Object(&array), size(w), size(h)])
            .and_then(|v| v.l())
            .map_err(|e| crate::java_error(env, e))?;
        let found = env.cast_local::<JFloatArray>(found).map_err(|e| e.to_string())?;
        // The Java side finds at most 20.
        let len = found.len(env).map_err(|e| e.to_string())?.min(60);
        let mut flat = vec![0.; len];
        found.get_region(env, 0, &mut flat).map_err(|e| e.to_string())?;
        Ok(flat)
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(face_strokes(&found, base.width() as f32, base.height() as f32))
}

#[cfg(not(target_os = "android"))]
fn find_faces(_: &RgbImage) -> anyhow::Result<Vec<Stroke>> {
    anyhow::bail!("no face detector here")
}

impl BeeperApp {
    pub(super) fn open_editor(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        let Some(entry) = p.entries.get(ix).filter(|e| !e.video && !e.failed && p.editor.is_none()) else { return };
        let (source, edits) = match &entry.edited {
            Some((source, edits)) => (source.clone(), edits.clone()),
            None => (entry.path.clone(), Edits::default()),
        };
        if !editable(&source) {
            return;
        }
        let entry = entry.id;
        p.next_id += 1;
        let serial = p.next_id;
        let field = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 3).submit_on_enter(true).placeholder(t!("Add text")));
        crate::ime::follow(&field, window, cx);
        let subs = vec![cx.subscribe_in(&field, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.add_label(window, cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        })];
        p.editor = Some(Editor {
            entry,
            serial,
            source: source.clone(),
            edits,
            tool: Tool::Move,
            color: 0,
            size: 1,
            base: None,
            shown: None,
            drawing: false,
            stale: false,
            drag: None,
            typing: false,
            saving: false,
            finding: false,
            field,
            rect: Rc::new(Cell::new(Bounds::default())),
            _subs: subs,
        });
        cx.spawn(async move |this, cx| {
            let base = cx.background_spawn(async move { load_base(&source) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(ed) = this.preview.editor.as_mut().filter(|e| e.serial == serial) else { return };
                match base {
                    Ok(base) => {
                        ed.base = Some(Arc::new(base));
                        this.redraw_edit(cx);
                    }
                    Err(e) => {
                        log::warn!("editor: {e:#}");
                        this.error = Some(t!("Could not open this photo").into());
                        this.close_editor(cx);
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn close_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(ed) = self.preview.editor.take()
            && let Some((image, _)) = ed.shown
        {
            self.released.push(image, cx);
        }
        cx.notify();
    }

    /// Draws the picture again in the background; a change meanwhile waits for that drawing.
    fn redraw_edit(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.preview.editor.as_mut() else { return };
        let Some(base) = ed.base.clone() else { return };
        if ed.drawing {
            ed.stale = true;
            return;
        }
        (ed.drawing, ed.stale) = (true, false);
        // Labels are painted over the picture by the stage, so they can move.
        let mut edits = Edits::default();
        (edits.frame, edits.strokes) = (ed.view_frame(), ed.edits.strokes.clone());
        let serial = ed.serial;
        cx.spawn(async move |this, cx| {
            let drawn = cx
                .background_spawn(async move {
                    let picture = paint::render((*base).clone(), &edits, prepare::PREVIEW_PX, &mut |_, _, _, _| {});
                    let size = (picture.width() as f32, picture.height() as f32);
                    bmp(picture).map(|bytes| (bytes, size))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(ed) = this.preview.editor.as_mut().filter(|e| e.serial == serial) else { return };
                ed.drawing = false;
                match drawn {
                    Ok((bytes, size)) => {
                        if let Some((old, _)) = ed.shown.replace((Arc::new(Image::from_bytes(ImageFormat::Bmp, bytes)), size)) {
                            this.released.push(old, cx);
                        }
                    }
                    Err(e) => log::warn!("editor: {e:#}"),
                }
                if this.preview.editor.as_ref().is_some_and(|e| e.stale) {
                    this.redraw_edit(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.preview.editor.as_mut() else { return };
        let was = ed.tool;
        ed.tool = if was == tool { Tool::Move } else { tool };
        ed.drag = None;
        ed.typing = ed.tool == Tool::Text;
        if ed.typing {
            ed.field.update(cx, |f, cx| f.focus(window, cx));
        } else if was == Tool::Text {
            window.blur(cx);
        }
        if (was == Tool::Crop) != (ed.tool == Tool::Crop) {
            self.redraw_edit(cx);
        }
        cx.notify();
    }

    fn add_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.preview.editor.as_mut() else { return };
        let text = ed.field.read(cx).value().to_string();
        let label = Label { text, at: ed.edits.frame.original_of([0.5, 0.5]), size: TEXT[ed.size], color: COLORS[ed.color] };
        if !ed.edits.add_label(label) && ed.edits.full() {
            self.error = Some(t!("No more room for changes on this photo").into());
        }
        ed.field.update(cx, |f, cx| f.set_value("", window, cx));
        (ed.typing, ed.tool) = (false, Tool::Move);
        window.blur(cx);
        cx.notify();
    }

    /// Blurs every face the device's detector finds, one stroke each, so undo takes them back one
    /// at a time.
    fn blur_faces(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.preview.editor.as_mut().filter(|e| !e.finding && !e.saving) else { return };
        let Some(base) = ed.base.clone() else { return };
        ed.finding = true;
        let serial = ed.serial;
        cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async move { find_faces(&base) }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(ed) = this.preview.editor.as_mut().filter(|e| e.serial == serial) else { return };
                ed.finding = false;
                match found {
                    Ok(faces) if faces.is_empty() => this.error = Some(t!("No faces found").into()),
                    Ok(faces) => {
                        if !faces.into_iter().all(|face| ed.edits.add_stroke(face)) {
                            this.error = Some(t!("No more room for changes on this photo").into());
                        }
                        this.redraw_edit(cx);
                    }
                    Err(e) => {
                        log::warn!("editor faces: {e:#}");
                        this.error = Some(t!("Could not look for faces").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn undo_edit(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.preview.editor.as_mut() else { return };
        ed.drag = None;
        ed.edits.undo();
        self.redraw_edit(cx);
        cx.notify();
    }

    fn turn_edit(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.preview.editor.as_mut() else { return };
        let before = ed.edits.frame;
        ed.edits.frame.rotate();
        ed.edits.framed(before);
        self.redraw_edit(cx);
        cx.notify();
    }

    /// A finger or the mouse on the picture; true when the editor took it.
    fn edit_pointer(&mut self, phase: TouchPhase, at: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(ed) = self.preview.editor.as_mut() else { return false };
        let rect = ed.rect.get();
        let (w, h) = (f32::from(rect.size.width), f32::from(rect.size.height));
        if w < 1. || h < 1. || ed.saving || ed.base.is_none() {
            return false;
        }
        let view = [f32::from(at.x - rect.origin.x) / w, f32::from(at.y - rect.origin.y) / h];
        let frame = ed.view_frame();
        match phase {
            TouchPhase::Started => {
                ed.drag = match ed.tool {
                    Tool::Pen | Tool::Blur if !ed.edits.full() => {
                        let (ink, width) = if ed.tool == Tool::Pen { (Ink::Pen(COLORS[ed.color]), PEN[ed.size]) } else { (Ink::Blur, BLUR[ed.size]) };
                        let mut stroke = Stroke { ink, width, points: Vec::new() };
                        stroke.extend(frame.original_of(view));
                        Some(Drag::Stroke(stroke))
                    }
                    Tool::Pen | Tool::Blur => {
                        self.error = Some(t!("No more room for changes on this photo").into());
                        None
                    }
                    Tool::Crop => {
                        let [x0, y0, x1, y1] = ed.edits.frame.crop;
                        let corners = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]];
                        let near = |c: [f32; 2]| ((c[0] - view[0]) * w).hypot((c[1] - view[1]) * h);
                        let corner = (0..4).filter(|&i| near(corners[i]) < GRAB).min_by(|&a, &b| near(corners[a]).total_cmp(&near(corners[b])));
                        match corner {
                            Some(i) => Some(Drag::Corner(i, ed.edits.frame)),
                            None if (x0..=x1).contains(&view[0]) && (y0..=y1).contains(&view[1]) => Some(Drag::Crop(view, ed.edits.frame)),
                            None => None,
                        }
                    }
                    Tool::Move | Tool::Text => {
                        let unit = frame.unit(w, h);
                        ed.edits.labels.iter().enumerate().rev().find_map(|(ix, label)| {
                            let c = frame.view_of(label.at);
                            let size = paint::label_px(label.size, unit, w, h);
                            let half_w = (label.text.chars().count() as f32 * size * 0.3).max(size / 2.);
                            let hit = ((c[0] - view[0]) * w).abs() <= half_w && ((c[1] - view[1]) * h).abs() <= size * 0.6;
                            hit.then_some(Drag::Label(ix, label.at, [c[0] - view[0], c[1] - view[1]]))
                        })
                    }
                };
            }
            TouchPhase::Moved => match &mut ed.drag {
                Some(Drag::Stroke(stroke)) => {
                    stroke.extend(frame.original_of(view));
                }
                Some(Drag::Corner(i, _)) => {
                    let c = &mut ed.edits.frame.crop;
                    let (x, y) = (view[0].clamp(0., 1.), view[1].clamp(0., 1.));
                    if *i % 2 == 0 { c[0] = x.min(c[2] - paint::MIN_CROP) } else { c[2] = x.max(c[0] + paint::MIN_CROP) }
                    if *i < 2 { c[1] = y.min(c[3] - paint::MIN_CROP) } else { c[3] = y.max(c[1] + paint::MIN_CROP) }
                }
                Some(Drag::Crop(start, before)) => {
                    let [x0, y0, x1, y1] = before.crop;
                    let dx = (view[0] - start[0]).clamp(-x0, 1. - x1);
                    let dy = (view[1] - start[1]).clamp(-y0, 1. - y1);
                    ed.edits.frame.crop = [x0 + dx, y0 + dy, x1 + dx, y1 + dy];
                }
                Some(Drag::Label(ix, _, offset)) => {
                    let to = [(view[0] + offset[0]).clamp(0., 1.), (view[1] + offset[1]).clamp(0., 1.)];
                    if let Some(label) = ed.edits.labels.get_mut(*ix) {
                        label.at = frame.original_of(to);
                    }
                }
                None => return false,
            },
            TouchPhase::Ended => match ed.drag.take() {
                Some(Drag::Stroke(stroke)) => {
                    ed.edits.add_stroke(stroke);
                    self.redraw_edit(cx);
                }
                Some(Drag::Corner(_, before) | Drag::Crop(_, before)) => {
                    ed.edits.frame.crop = paint::clamp_crop(ed.edits.frame.crop);
                    ed.edits.framed(before);
                }
                Some(Drag::Label(ix, from, _)) => ed.edits.moved(ix, from),
                None => return false,
            },
            TouchPhase::Cancelled => match ed.drag.take() {
                Some(Drag::Corner(_, before) | Drag::Crop(_, before)) => ed.edits.frame = before,
                Some(Drag::Label(ix, from, _)) => {
                    if let Some(label) = ed.edits.labels.get_mut(ix) {
                        label.at = from;
                    }
                }
                Some(Drag::Stroke(_)) => {}
                None => return false,
            },
        }
        cx.notify();
        true
    }

    /// Writes the edited picture and puts it in the item's place; no edits put the original back.
    fn finish_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.editor.as_ref().is_some_and(|e| e.typing) {
            self.add_label(window, cx);
        }
        let Some(ed) = self.preview.editor.as_mut().filter(|e| !e.saving && e.base.is_some()) else { return };
        let (entry, serial) = (ed.entry, ed.serial);
        if ed.edits.is_empty() {
            self.set_edited(entry, None, cx);
            return self.close_editor(cx);
        }
        let shaped: Vec<Shaped> = ed.edits.labels.iter().map(|l| shape(l, window)).collect();
        let text_system = cx.text_system().clone();
        let (source, edits) = (ed.source.clone(), ed.edits.clone());
        ed.saving = true;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let folder = self.account_dir().join(crate::backend::PASTED).join(format!("{stamp}-edit-{serial}"));
        cx.spawn(async move |this, cx| {
            let made = {
                let edits = edits.clone();
                let source = source.clone();
                cx.background_spawn(async move { save(&source, &edits, &shaped, &text_system, &folder) }).await
            };
            let _ = this.update(cx, |this, cx| {
                let open = this.preview.editor.as_ref().is_some_and(|e| e.serial == serial);
                match made {
                    Ok(path) if open => {
                        this.set_edited(entry, Some((path, source, edits)), cx);
                        this.close_editor(cx);
                    }
                    Ok(path) => this.drop_copy(&path, cx),
                    Err(e) => {
                        log::warn!("editor: {e:#}");
                        this.error = Some(t!("Could not save the edited photo").into());
                        if let Some(ed) = this.preview.editor.as_mut().filter(|_| open) {
                            ed.saving = false;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Puts an edited copy (its path, the original's, the edits) in the item's place, or the
    /// original back; the copy it had goes.
    fn set_edited(&mut self, entry: u64, made: Option<(PathBuf, PathBuf, Edits)>, cx: &mut Context<Self>) {
        let Some(e) = self.preview.entries.iter_mut().find(|e| e.id == entry) else {
            if let Some((path, _, _)) = made {
                self.drop_copy(&path, cx);
            }
            return;
        };
        let previous = e.edited.take();
        let old = previous.as_ref().map(|_| e.path.clone());
        match (made, previous) {
            (Some((path, source, edits)), _) => {
                e.path = path;
                e.edited = Some((source, edits));
            }
            (None, Some((source, _))) => e.path = source,
            (None, None) => return,
        }
        let images: Vec<_> = [e.strip.take(), e.big.take()].into_iter().flatten().collect();
        e.failed = false;
        for image in images {
            self.released.push(image, cx);
        }
        if let Some(old) = old {
            self.drop_copy(&old, cx);
        }
        self.pump_preview(cx);
    }

    /// Deletes an edited copy's folder; only ever one the editor made in `pasted`.
    pub(super) fn drop_copy(&self, path: &Path, cx: &mut Context<Self>) {
        let pasted = self.account_dir().join(crate::backend::PASTED);
        let Some(folder) = path.parent().filter(|f| f.parent() == Some(pasted.as_path())).map(Path::to_path_buf) else { return };
        cx.background_spawn(async move {
            if let Err(e) = std::fs::remove_dir_all(folder) {
                log::debug!("edited copy: {e}");
            }
        })
        .detach();
    }

    pub(super) fn render_editor(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ed = self.preview.editor.as_ref()?;
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let (white, black) = (gpui_kit::white(), gpui_kit::black());
        let can_undo = ed.edits.can_undo() && !ed.saving;

        let bar = h_flex()
            .h(px(TAP))
            .flex_none()
            .px(px(GUTTER - 10.))
            .justify_between()
            .text_color(white)
            .child(round_button("edit-close", IconName::X, cx).on_click(cx.listener(|this, _, window, cx| {
                window.blur(cx);
                this.close_editor(cx);
            })))
            .child(
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(
                        round_button("edit-undo", IconName::Undo2, cx)
                            .when(!can_undo, |el| el.opacity(0.4))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if can_undo {
                                    this.undo_edit(cx);
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("edit-done")
                            .h(px(36.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .text_size(fs(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .bg(white)
                            .text_color(black)
                            .cursor_pointer()
                            .when(ed.saving, |el| el.opacity(0.6))
                            .on_click(cx.listener(|this, _, window, cx| this.finish_edit(window, cx)))
                            .child(if ed.saving { t!("Saving…") } else { t!("Done") }),
                    ),
            );

        let overlay = Overlay {
            frame: ed.view_frame(),
            labels: if ed.tool == Tool::Crop { Vec::new() } else { ed.edits.labels.clone() },
            stroke: match &ed.drag {
                Some(Drag::Stroke(s)) => Some(s.clone()),
                _ => None,
            },
            crop: (ed.tool == Tool::Crop).then_some(ed.edits.frame.crop),
            size: ed.shown.as_ref().map(|(_, size)| *size),
            rect: ed.rect.clone(),
        };
        let picture = div()
            .relative()
            .size_full()
            .when_some(ed.shown.as_ref().map(|(image, _)| image.clone()), |el, image| el.child(img(image).absolute().inset_0().size_full().object_fit(ObjectFit::Contain)))
            .when(ed.shown.is_none(), |el| el.flex().items_center().justify_center().text_color(p.faint).text_size(fs(14.)).child(t!("Loading…")))
            .child(overlay.element(cx.entity().downgrade()));
        let stage = div().flex_1().min_h_0().p(px(GUTTER)).child(picture);

        let tool_button = |id: &'static str, tool: Tool, icon: IconName| {
            let on = ed.tool == tool;
            div()
                .id(id)
                .size(px(TAP))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .when(on, |el| el.bg(white).text_color(black))
                .when(!on, |el| el.text_color(white).hover(|el| el.bg(white.opacity(0.15))))
                .on_click(cx.listener(move |this, _, window, cx| this.pick_tool(tool, window, cx)))
                .child(Icon::new(icon).size_5())
        };
        let tools = h_flex()
            .flex_none()
            .h(px(TAP + 12.))
            .gap(px(14.))
            .justify_center()
            .items_center()
            .child(tool_button("edit-crop", Tool::Crop, IconName::Crop))
            .child(tool_button("edit-pen", Tool::Pen, IconName::Pencil))
            .child(tool_button("edit-text", Tool::Text, IconName::Type))
            .child(tool_button("edit-blur", Tool::Blur, IconName::Droplets));

        let swatches = matches!(ed.tool, Tool::Pen | Tool::Text).then(|| {
            // Eight targets need 366 px; a phone has about 328, so they narrow rather than overflow.
            h_flex().gap(px(2.)).children(COLORS.iter().enumerate().map(|(ix, &c)| {
                let on = ix == ed.color;
                div()
                    .id(("edit-color", ix))
                    .w(px(TAP))
                    .h(px(TAP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ed) = this.preview.editor.as_mut() {
                            ed.color = ix;
                            cx.notify();
                        }
                    }))
                    .child(div().size(px(if on { 30. } else { 24. })).rounded_full().bg(hsla(c)).border_2().border_color(if on { white } else { white.opacity(0.3) }))
            }))
        });
        let sizes = matches!(ed.tool, Tool::Pen | Tool::Text | Tool::Blur).then(|| {
            h_flex().children([8., 14., 20.].into_iter().enumerate().map(|(ix, dot)| {
                let on = ix == ed.size;
                div()
                    .id(("edit-size", ix))
                    .size(px(TAP))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ed) = this.preview.editor.as_mut() {
                            ed.size = ix;
                            cx.notify();
                        }
                    }))
                    .child(div().size(px(dot)).rounded_full().bg(if on { white } else { white.opacity(0.4) }))
            }))
        });
        // Only Android has a face detector without a new crate.
        let faces = (cfg!(target_os = "android") && ed.tool == Tool::Blur).then(|| {
            div()
                .id("edit-faces")
                .h(px(36.))
                .px(px(14.))
                .flex()
                .flex_none()
                .items_center()
                .rounded_full()
                .text_size(fs(14.))
                .text_color(white)
                .bg(white.opacity(0.15))
                .cursor_pointer()
                .when(ed.finding, |el| el.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.blur_faces(cx)))
                .child(t!("Blur faces"))
        });
        let turn = (ed.tool == Tool::Crop).then(|| {
            round_button("edit-turn", IconName::RotateCw, cx).text_color(white).on_click(cx.listener(|this, _, _, cx| this.turn_edit(cx)))
        });
        let options = h_flex()
            .id("edit-options")
            .flex_none()
            .min_h(px(TAP))
            .px(px(GUTTER))
            .gap(px(8.))
            .flex_wrap()
            .justify_center()
            .items_center()
            .children(swatches)
            .children(sizes)
            .children(faces)
            .children(turn);

        let typing = ed.typing.then(|| {
            h_flex()
                .flex_none()
                .px(px(GUTTER))
                .py(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h(px(TAP))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .rounded(px(TAP / 2.))
                        .overflow_hidden()
                        .bg(p.surface)
                        .child(Textarea::new(&ed.field).appearance(false).text_size(fs(16.)))
                        .child(crate::ime::marks(&ed.field)),
                )
        });

        Some(
            v_flex()
                .id("editor")
                .absolute()
                .inset_0()
                .pt(px(top))
                .pb(px(bottom))
                .bg(black)
                .occlude()
                .child(bar)
                .child(stage)
                .children(typing)
                .child(options)
                .child(tools)
                .into_any_element(),
        )
    }
}

/// What the stage paints over the picture: labels, the stroke under the finger, the crop.
struct Overlay {
    frame: Frame,
    labels: Vec<Label>,
    stroke: Option<Stroke>,
    crop: Option<[f32; 4]>,
    size: Option<(f32, f32)>,
    rect: Rc<Cell<Bounds<Pixels>>>,
}

impl Overlay {
    fn element(self, view: WeakEntity<BeeperApp>) -> impl IntoElement {
        canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                let Some(size) = self.size else { return };
                let rect = contain(bounds, size);
                self.rect.set(rect);
                self.paint(rect, window, cx);
                listen(view, bounds, window, BeeperApp::edit_pointer);
            },
        )
        .absolute()
        .inset_0()
    }

    fn paint(&self, rect: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let (w, h) = (f32::from(rect.size.width), f32::from(rect.size.height));
        let at = |p: [f32; 2]| point(rect.origin.x + px(p[0] * w), rect.origin.y + px(p[1] * h));
        let unit = self.frame.unit(w, h);
        window.with_content_mask(Some(ContentMask { bounds: rect }), |window| {
            for label in &self.labels {
                let size = paint::label_px(label.size, unit, w, h);
                let run = TextRun { len: label.text.len(), font: label_font(), color: hsla(label.color), background_color: None, underline: None, strikethrough: None };
                let line = window.text_system().shape_line(label.text.clone().into(), px(size), &[run], None);
                let c = at(self.frame.view_of(label.at));
                let origin = point(c.x - line.width / 2., c.y - px(size / 2.));
                if let Err(e) = line.paint(origin, px(size), TextAlign::Left, None, window, cx) {
                    log::debug!("editor label: {e}");
                }
            }
            if let Some(stroke) = &self.stroke {
                let color = match stroke.ink {
                    Ink::Pen(c) => hsla(c),
                    Ink::Blur => gpui_kit::white().opacity(0.5),
                };
                let mut path = PathBuilder::stroke(px(stroke.width * unit));
                for (ix, &p) in stroke.points.iter().enumerate() {
                    let p = at(self.frame.view_of(p));
                    if ix == 0 { path.move_to(p) } else { path.line_to(p) }
                }
                if stroke.points.len() > 1
                    && let Ok(path) = path.build()
                {
                    window.paint_path(path, color);
                }
            }
        });
        if let Some([x0, y0, x1, y1]) = self.crop {
            let shade = gpui_kit::black().opacity(0.6);
            let (a, b) = (at([x0, y0]), at([x1, y1]));
            let (left, right, top, bottom) = (rect.origin.x, rect.origin.x + rect.size.width, rect.origin.y, rect.origin.y + rect.size.height);
            for (from, to) in [(point(left, top), point(right, a.y)), (point(left, b.y), point(right, bottom)), (point(left, a.y), point(a.x, b.y)), (point(b.x, a.y), point(right, b.y))] {
                window.paint_quad(fill(Bounds::from_corners(from, to), shade));
            }
            window.paint_quad(outline(Bounds::from_corners(a, b), gpui_kit::white(), BorderStyle::Solid));
            for c in [a, point(b.x, a.y), point(a.x, b.y), b] {
                let knob = Bounds::new(point(c.x - px(8.), c.y - px(8.)), size(px(16.), px(16.)));
                window.paint_quad(fill(knob, gpui_kit::white()).corner_radii(px(8.)));
            }
        }
    }
}

type Pointer = fn(&mut BeeperApp, TouchPhase, Point<Pixels>, &mut Context<BeeperApp>) -> bool;

/// Fingers and the mouse within `bounds` go to `on`, which says whether it took them. A touch is
/// claimed before it becomes a pan, as a scrollbar's thumb claims it.
pub(super) fn listen(view: WeakEntity<BeeperApp>, bounds: Bounds<Pixels>, window: &mut Window, on: Pointer) {
    let touch = view.clone();
    window.on_mouse_event(move |e: &TouchDragEvent, phase, window, cx| {
        if !phase.bubble() {
            return;
        }
        if e.phase == TouchPhase::Started && (window.default_prevented() || !bounds.contains(&e.start_position)) {
            return;
        }
        let at = if e.phase == TouchPhase::Started { e.start_position } else { e.position };
        let taken = touch
            .update(cx, |this, cx| {
                this.preview.touched = Some(Instant::now());
                on(this, e.phase, at, cx)
            })
            .unwrap_or(false);
        if taken {
            window.prevent_default();
            cx.stop_propagation();
        }
    });
    // A tap's own mouse events follow its touch; only a real mouse draws through these.
    let by_mouse = |this: &BeeperApp| this.preview.touched.is_none_or(|t| t.elapsed() > Duration::from_secs(1));
    let down = view.clone();
    window.on_mouse_event(move |e: &MouseDownEvent, phase, _, cx| {
        if phase.bubble() && e.button == MouseButton::Left && bounds.contains(&e.position) {
            let _ = down.update(cx, |this, cx| by_mouse(this) && on(this, TouchPhase::Started, e.position, cx));
        }
    });
    let moved = view.clone();
    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
        if phase.bubble() && e.pressed_button == Some(MouseButton::Left) {
            let _ = moved.update(cx, |this, cx| by_mouse(this) && on(this, TouchPhase::Moved, e.position, cx));
        }
    });
    window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
        if phase.bubble() && e.button == MouseButton::Left {
            let _ = view.update(cx, |this, cx| by_mouse(this) && on(this, TouchPhase::Ended, e.position, cx));
        }
    });
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{Ink, face_strokes};

    #[test]
    fn a_found_face_becomes_a_blur_over_it() {
        let strokes = face_strokes(&[400., 300., 50., 10., 10., 0.], 800., 600.);
        assert_eq!(strokes.len(), 1, "a face with no eye distance is dropped");
        let face = &strokes[0];
        assert_eq!(face.ink, Ink::Blur);
        assert_eq!(face.points, vec![[0.5, 320. / 600.]]);
        assert!((face.width - 0.2).abs() < 1e-6);
    }
}
