//! Settings, Appearance: the text size and the wallpaper behind the messages of every chat. A chat
//! may have its own wallpaper and colour (`chatlook`), which is drawn in its place.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use gpui_kit::component::Theme;
use gpui_kit::component::slider::{Slider, SliderState};

pub(super) use gpui_kit::component::slider::SliderEvent;

use super::panels::row;
use super::*;
use crate::i18n::t;

/// How large text is drawn, as a share of the design's sizes. Applies to every `fs` call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum TextSize {
    Small,
    #[default]
    Standard,
    Large,
    Larger,
    Largest,
}

impl TextSize {
    const ALL: [Self; 5] = [Self::Small, Self::Standard, Self::Large, Self::Larger, Self::Largest];

    pub(crate) fn percent(self) -> u32 {
        match self {
            Self::Small => 85,
            Self::Standard => 100,
            Self::Large => 110,
            Self::Larger => 120,
            Self::Largest => 130,
        }
    }
}

static SCALE: AtomicU32 = AtomicU32::new(100);

/// A font size or line height of the design, at the chosen text size.
pub(crate) fn fs(size: f32) -> Pixels {
    px(size * SCALE.load(Ordering::Relaxed) as f32 / 100.)
}

/// What is drawn behind the messages. Presets are tones of the dark `Palette`: none is light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Wallpaper {
    #[default]
    None,
    Graphite,
    Slate,
    Carbon,
    Photo,
}

impl Wallpaper {
    pub(super) const PRESETS: [Self; 4] = [Self::None, Self::Graphite, Self::Slate, Self::Carbon];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::None => t!("Black"),
            Self::Graphite => t!("Graphite"),
            Self::Slate => t!("Slate"),
            Self::Carbon => t!("Carbon"),
            Self::Photo => t!("Photo"),
        }
    }

    /// `None` for the plain ground. At most `raised` darkened to 85%, so `faint` text keeps 5:1.
    pub(super) fn fill(self, p: &Palette) -> Option<Background> {
        let tone = |t: f32| mix(p.ground, p.raised, t);
        let stop = linear_color_stop;
        match self {
            Self::None | Self::Photo => None,
            Self::Graphite => Some(tone(0.6).into()),
            Self::Slate => Some(linear_gradient(180., stop(tone(0.85), 0.), stop(p.ground, 1.))),
            Self::Carbon => Some(linear_gradient(135., stop(p.ground, 0.), stop(tone(0.8), 1.))),
        }
    }
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (a.into(), b.into());
    let at = |a: f32, b: f32| a + (b - a) * t;
    Rgba { r: at(a.r, b.r), g: at(a.g, b.g), b: at(a.b, b.b), a: 1. }.into()
}

/// The dim a photo gets, in percent of black laid over it.
pub(crate) const DIM_MIN: u8 = 30;
const DIM_MAX: u8 = 85;
pub(crate) fn default_dim() -> u8 {
    50
}

/// A photo is darkened to this share when it is imported, so that with the least dim the
/// brightest it can get is `#6b6b6b`, behind which white text still reads at 5:1.
const BAKE: f32 = 0.6;
/// The file in the data directory.
const PHOTO: &str = "wallpaper.jpg";
/// Longest side of the stored picture: 2400 x 1080 on a phone is 10 MB decoded, never above 23 MB.
const MAX_SIDE: u32 = 2400;
/// Decoded sources above this would take more than ~400 MB while they are scaled down.
const SOURCE_PX: u32 = 12_000;

/// Copies `src` to `dest` as a picture of `size`, covering it, upright and darkened. Runs once
/// per chosen photo; only the result is kept and decoded afterwards.
pub(super) fn import(src: &Path, dest: &Path, size: (u32, u32)) -> Result<Vec<u8>, String> {
    use image::ImageDecoder as _;
    let (w, h) = (size.0.clamp(64, MAX_SIDE), size.1.clamp(64, MAX_SIDE));
    let mut reader = image::ImageReader::open(src).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    (limits.max_image_width, limits.max_image_height) = (Some(SOURCE_PX), Some(SOURCE_PX));
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut picture = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    picture.apply_orientation(orientation);
    let mut rgb = picture.resize_to_fill(w, h, image::imageops::FilterType::Triangle).to_rgb8();
    drop(picture);
    for channel in rgb.iter_mut() {
        *channel = (f32::from(*channel) * BAKE) as u8;
    }
    let mut out = std::io::Cursor::new(Vec::new());
    rgb.write_to(&mut out, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
    let out = out.into_inner();
    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, &out).and_then(|()| std::fs::rename(&tmp, dest)).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The appearance state that is not a setting itself.
pub(super) struct Look {
    photo: Option<Arc<Image>>,
    /// The open chat's own photo, decoded while that chat is open.
    pub(super) chat_photo: Option<(OwnedRoomId, Arc<Image>)>,
    /// The chat's sheet shows one of the chat's own settings pages.
    pub(super) chat_page: Option<super::chatlook::ChatPage>,
    pub(super) dim: Entity<SliderState>,
    importing: bool,
    error: Option<String>,
    /// A sideways pan on the dim slider: where it began and how far it has moved, in px.
    slide: Option<(f32, f32)>,
}

/// Where a pan that began at `start` and moved `moved` puts the finger along a track at `left`,
/// as a share of its `width`; `None` before the track has been laid out.
fn pan_share(start: f32, moved: f32, left: f32, width: f32) -> Option<f32> {
    (width > 0.).then(|| ((start + moved - left) / width).clamp(0., 1.))
}

impl Look {
    pub(super) fn new(settings: &crate::Settings, dir: &Path, cx: &mut Context<BeeperApp>) -> Self {
        SCALE.store(settings.text_size.percent(), Ordering::Relaxed);
        let photo = (settings.wallpaper == Wallpaper::Photo)
            .then(|| std::fs::read(dir.join(PHOTO)).ok())
            .flatten()
            .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes)));
        let dim = f32::from(settings.wallpaper_dim.clamp(DIM_MIN, DIM_MAX));
        let dim = cx.new(|_| SliderState::new().min(f32::from(DIM_MIN)).max(f32::from(DIM_MAX)).step(5.).default_value(dim));
        Self { photo, chat_photo: None, chat_page: None, dim, importing: false, error: None, slide: None }
    }
}

impl BeeperApp {
    /// The open chat's wallpaper and its photo: the chat's own, else the one for all chats.
    fn chat_paper(&self) -> (Wallpaper, Option<&Arc<Image>>) {
        match self.open.as_ref().and_then(|id| self.settings.chat_looks.get(id)).and_then(|look| look.wallpaper) {
            Some(Wallpaper::Photo) => (Wallpaper::Photo, self.look.chat_photo.as_ref().filter(|(id, _)| self.open.as_ref() == Some(id)).map(|(_, photo)| photo)),
            Some(own) => (own, None),
            None => (self.settings.wallpaper, self.look.photo.as_ref()),
        }
    }

    /// The colour of small text that sits straight on the open chat's wallpaper (day labels, times
    /// beneath a message): `fallback` where there is none or it is a preset, white on a photo, whose
    /// brightest point is only bounded, not dark.
    pub(super) fn on_wallpaper(&self, fallback: Hsla, cx: &App) -> Hsla {
        let (paper, photo) = self.chat_paper();
        ink_on_paper(paper, photo, fallback, cx)
    }

    /// The open chat's wallpaper, to be placed first in a box that has `relative()`.
    pub(super) fn wallpaper_layer(&self, radius: f32, cx: &App) -> Option<AnyElement> {
        let (paper, photo) = self.chat_paper();
        paper_layer(paper, photo, self.settings.wallpaper_dim, radius, cx)
    }

    fn set_text_size(&mut self, size: TextSize, cx: &mut Context<Self>) {
        self.settings.text_size = size;
        self.settings.save(&self.dir);
        SCALE.store(size.percent(), Ordering::Relaxed);
        // Rows of the open chat were measured at the old size.
        self.timeline.remeasure();
        cx.notify();
    }

    pub(super) fn set_wallpaper(&mut self, wallpaper: Wallpaper, cx: &mut Context<Self>) {
        self.settings.wallpaper = wallpaper;
        self.settings.save(&self.dir);
        cx.notify();
    }

    pub(super) fn dim_changed(&mut self, ev: &SliderEvent, cx: &mut Context<Self>) {
        let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
        self.settings.wallpaper_dim = v.end().round().clamp(f32::from(DIM_MIN), f32::from(DIM_MAX)) as u8;
        // The file is written once, when the finger lifts.
        if matches!(ev, SliderEvent::Release(_)) {
            self.settings.save(&self.dir);
        }
        cx.notify();
    }

    /// A finger never makes mouse moves (it pans), so the slider's own drag never starts on a
    /// phone: a sideways pan over it is turned into the positions a mouse drag would give.
    fn dim_pan(&mut self, e: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let d = e.delta.pixel_delta(px(20.));
        let (dx, sideways) = (f32::from(d.x), d.y.is_zero() && !d.x.is_zero());
        match (e.touch_phase, self.look.slide.as_mut()) {
            // The recognizer locks a pan to one axis, so a sideways one has no vertical part.
            (TouchPhase::Started, _) if sideways => self.look.slide = Some((f32::from(e.position.x), dx)),
            (TouchPhase::Started, _) => self.look.slide = None,
            (_, Some(slide)) => slide.1 += dx,
            (_, None) => return,
        }
        cx.stop_propagation();
        let Some((start, moved)) = self.look.slide else { return };
        let ended = matches!(e.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled);
        self.look.dim.update(cx, |s, cx| {
            let track = s.bounds();
            if let Some(share) = pan_share(start, moved, f32::from(track.left()), f32::from(track.size.width)) {
                let at = point(track.left() + track.size.width * share, track.center().y);
                s.update_value_by_position(Axis::Horizontal, at, false, window, cx);
            }
            if ended {
                s.handle_release(cx);
            }
        });
        if ended {
            self.look.slide = None;
        }
    }

    /// Picks a photo for all chats, or with `room` for that chat alone.
    pub(super) fn choose_photo(&mut self, room: Option<OwnedRoomId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.look.importing {
            return;
        }
        let dest = match &room {
            Some(_) => match super::chatlook::photo_path(&self.dir, &super::chatlook::new_photo_name()) {
                Some(dest) => dest,
                None => return,
            },
            None => self.dir.join(PHOTO),
        };
        let size = window.viewport_size();
        let scale = window.scale_factor();
        let size = ((f32::from(size.width) * scale) as u32, (f32::from(size.height) * scale) as u32);
        let picked = crate::pick_file(cx);
        self.look.importing = true;
        self.look.error = None;
        cx.spawn(async move |this, cx| {
            let imported = match picked.await {
                Some(path) => Some(
                    cx.background_spawn(async move {
                        if let Some(parent) = dest.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        import(&path, &dest, size).map(|bytes| (bytes, dest))
                    })
                    .await,
                ),
                None => None,
            };
            let _ = this.update(cx, |this, cx| {
                this.look.importing = false;
                match imported {
                    Some(Ok((bytes, dest))) => {
                        let image = Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes));
                        match room {
                            Some(room) => this.set_chat_photo(room, &dest, image, cx),
                            None => {
                                if let Some(old) = this.look.photo.replace(image) {
                                    this.released.push(old, cx);
                                }
                                this.set_wallpaper(Wallpaper::Photo, cx);
                            }
                        }
                    }
                    Some(Err(e)) => {
                        log::warn!("wallpaper: {e}");
                        let error = t!("That picture could not be used.");
                        // A chat's sheet has no line for it; the app's error bar shows it there.
                        if room.is_some() { this.error = Some(error.into()) } else { this.look.error = Some(error.into()) }
                    }
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Text size and Chat wallpaper rows of the Appearance card.
    pub(super) fn render_look(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = Palette::of(cx);
        let sizes = h_flex().p(px(4.)).rounded_full().bg(p.raised).children(TextSize::ALL.map(|size| {
            let on = self.settings.text_size == size;
            div()
                .id(SharedString::from(format!("text-size-{}", size.percent())))
                .h(px(36.))
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(fs(14.))
                .text_color(if on { p.ground } else { p.muted })
                .when(on, |el| el.bg(p.text))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_text_size(size, cx)))
                .child(format!("{}%", size.percent()))
        }));
        let bubble = |text: &'static str, own: bool| {
            let ink = if own { p.on_own } else { p.text };
            div()
                .max_w(relative(0.8))
                .px(px(GAP))
                .py(px(8.))
                .rounded(px(RADIUS))
                .bg(if own { p.own } else { p.surface })
                .text_color(ink)
                .text_size(fs(16.))
                .line_height(fs(22.))
                .child(text)
        };
        let (paper, photo) = (self.settings.wallpaper, self.look.photo.as_ref());
        let faint = ink_on_paper(paper, photo, p.faint, cx);
        let preview = v_flex()
            .relative()
            .overflow_hidden()
            .rounded(px(RADIUS))
            .bg(p.ground)
            .children(paper_layer(paper, photo, self.settings.wallpaper_dim, RADIUS, cx))
            .child(
                v_flex()
                    .p(px(GAP))
                    .gap(px(8.))
                    .child(bubble(t!("This is how messages look."), false))
                    .child(h_flex().justify_end().child(bubble(t!("And your replies."), true)))
                    .child(div().flex().justify_end().text_size(fs(12.)).text_color(faint).child("12:41")),
            );
        let mut swatches = h_flex().flex_wrap().gap_x(px(4.)).pb(px(4.));
        for wallpaper in Wallpaper::PRESETS {
            let fill = div().bg(wallpaper.fill(&p).unwrap_or_else(|| p.ground.into()));
            swatches = swatches.child(swatch(wallpaper.label(), self.settings.wallpaper == wallpaper, fill, cx).on_click(cx.listener(move |this, _, _, cx| this.set_wallpaper(wallpaper, cx))));
        }
        if let Some(photo) = self.look.photo.clone() {
            let fill = div().child(img(photo).size_full().object_fit(ObjectFit::Cover));
            swatches = swatches.child(swatch(t!("Photo"), self.settings.wallpaper == Wallpaper::Photo, fill, cx).on_click(cx.listener(|this, _, _, cx| this.set_wallpaper(Wallpaper::Photo, cx))));
        }
        let choose = if self.look.importing { t!("Working…") } else if self.look.photo.is_some() { t!("Change photo") } else { t!("Choose photo") };
        let choose = super::panels::link("choose-photo", choose, SharedString::default(), Some(IconName::Image), None, cx).on_click(cx.listener(|this, _, window, cx| this.choose_photo(None, window, cx)));
        let mut rows = vec![
            row(t!("Text size"), t!("Messages and the rest of the app."), cx).into_any_element(),
            div().pb(px(8.)).child(sizes).into_any_element(),
            div().pb(px(8.)).child(preview).into_any_element(),
            row(t!("Chat wallpaper"), t!("One for all chats, with the dark theme."), cx).into_any_element(),
            swatches.into_any_element(),
            choose.into_any_element(),
        ];
        if self.settings.wallpaper == Wallpaper::Photo && self.look.photo.is_some() {
            rows.push(row(t!("Dim photo"), "", cx).into_any_element());
            let slider = div().pb(px(12.)).px(px(4.)).child(Slider::new(&self.look.dim));
            // The row does not move during a sideways pan, so every step of it lands here.
            let slider = slider.when(cfg!(target_os = "android"), |el| el.on_scroll_wheel(cx.listener(Self::dim_pan)));
            rows.push(slider.into_any_element());
        }
        if let Some(e) = &self.look.error {
            rows.push(div().pb(px(8.)).text_size(fs(13.)).text_color(p.negative).child(e.clone()).into_any_element());
        }
        rows
    }
}

/// Whether a wallpaper is drawn: the presets are dark, so the light theme has none.
fn paper_on(paper: Wallpaper, photo: Option<&Arc<Image>>, cx: &App) -> bool {
    Theme::global(cx).is_dark()
        && match paper {
            Wallpaper::None => false,
            Wallpaper::Photo => photo.is_some(),
            _ => true,
        }
}

fn ink_on_paper(paper: Wallpaper, photo: Option<&Arc<Image>>, fallback: Hsla, cx: &App) -> Hsla {
    if paper == Wallpaper::Photo && paper_on(paper, photo, cx) { Palette::of(cx).text } else { fallback }
}

fn paper_layer(paper: Wallpaper, photo: Option<&Arc<Image>>, dim: u8, radius: f32, cx: &App) -> Option<AnyElement> {
    if !paper_on(paper, photo, cx) {
        return None;
    }
    let layer = div().absolute().inset_0().overflow_hidden().rounded(px(radius));
    Some(match (paper, photo) {
        (Wallpaper::Photo, Some(photo)) => layer
            .child(img(photo.clone()).size_full().rounded(px(radius)).object_fit(ObjectFit::Cover))
            .child(div().absolute().inset_0().rounded(px(radius)).bg(gpui_kit::black().opacity(f32::from(dim) / 100.)))
            .into_any_element(),
        (wallpaper, _) => layer.when_some(wallpaper.fill(&Palette::of(cx)), |el, fill| el.bg(fill)).into_any_element(),
    })
}

/// A round swatch that shows `fill`, ringed while `on`; the caller gives it its click.
pub(super) fn swatch(name: &str, on: bool, fill: Div, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    div()
        .id(SharedString::from(name.to_owned()))
        .size(px(TAP))
        .flex()
        .items_center()
        .justify_center()
        .role(Role::RadioButton)
        .aria_label(SharedString::from(name.to_owned()))
        .aria_toggled(on.into())
        .cursor_pointer()
        .child(
            div()
                .size(px(32.))
                .p(px(2.))
                .rounded_full()
                .border_2()
                .border_color(if on { p.text } else { p.raised })
                .child(fill.size_full().rounded_full().overflow_hidden()),
        )
}

#[cfg(test)]
mod tests {
    use super::{BAKE, DIM_MIN, PHOTO, TextSize, import, pan_share};

    #[test]
    fn pan_maps_the_finger_onto_the_track() {
        assert_eq!(pan_share(150., 0., 100., 200.), Some(0.25));
        assert_eq!(pan_share(150., 50., 100., 200.), Some(0.5));
        assert_eq!(pan_share(150., -80., 100., 200.), Some(0.));
        assert_eq!(pan_share(150., 400., 100., 200.), Some(1.));
        assert_eq!(pan_share(150., 10., 0., 0.), None);
    }

    #[test]
    fn five_steps_from_85_to_130() {
        let steps: Vec<u32> = TextSize::ALL.iter().map(|s| s.percent()).collect();
        assert_eq!(steps, [85, 100, 110, 120, 130]);
        assert_eq!(TextSize::default().percent(), 100);
    }

    /// Text on the least dimmed, brightest possible photo, and on the lightest preset.
    #[test]
    fn text_keeps_contrast_on_every_wallpaper() {
        let brightest = (255. * BAKE * (1. - f32::from(DIM_MIN) / 100.)) as u32;
        let photo = brightest << 16 | brightest << 8 | brightest;
        assert!(crate::contrast(photo, 0xffffff) >= 4.5, "{photo:x}");
        let preset = (0x2a as f32 * 0.85) as u32;
        let preset = preset << 16 | preset << 8 | preset;
        assert!(crate::contrast(preset, 0x949494) >= 4.5, "{preset:x}");
    }

    #[test]
    fn photo_is_stored_covering_and_dark() {
        let dir = std::env::temp_dir().join(format!("beeper-gpui-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.png");
        image::RgbImage::from_pixel(900, 300, image::Rgb([255, 255, 255])).save(&src).unwrap();
        let out = import(&src, &dir.join(PHOTO), (200, 400)).unwrap();
        let stored = image::load_from_memory(&out).unwrap().to_rgb8();
        assert_eq!(stored.dimensions(), (200, 400));
        assert!(stored.pixels().all(|px| px.0.iter().all(|&c| c <= (255. * BAKE) as u8 + 2)));
        assert!(dir.join(PHOTO).exists());
        assert!(import(&dir.join("missing.png"), &dir.join(PHOTO), (200, 400)).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
