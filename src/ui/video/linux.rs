//! The desktop's video player on Linux, through the system's GStreamer: `playbin` decodes, an
//! `appsink` hands each frame over as BGRA, and the newest one is drawn as a picture filling the
//! window. Play and pause, seek, mute, speed, and close with Esc or the X, as the Android player has
//! them; a double click on either side skips 10 s, each further click another 10 s.
//!
//! Cost while open: one decoded frame waits for the screen (width x height x 4 bytes, 8 MB at
//! 1080p) and one is on it as a texture; each new frame replaces both, so a video at 30 frames a
//! second copies and uploads 30 of them. A timer looks for a new frame every `TICK`, which only
//! reads a lock. Everything stops and goes when the player closes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;

use crate::backend::Media;
use crate::i18n::t;
use crate::ui::voice::SPEEDS;
use crate::ui::{BeeperApp, fs, length};

/// How often the player looks for a new frame and for the end: below a 120 Hz frame's time.
const TICK: Duration = Duration::from_millis(8);
/// What the arrow keys skip.
const SKIP: Duration = Duration::from_secs(5);
/// What a double click on either side skips.
const CLICK_SKIP: Duration = Duration::from_secs(10);
/// How long the skip label stays after the last skip.
const SKIP_SHOWN: Duration = Duration::from_millis(600);

/// A decoded frame: BGRA rows without padding.
struct Picture {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

pub(in crate::ui) struct Player {
    playbin: gst::Element,
    newest: Arc<Mutex<Option<Picture>>>,
    shown: Option<Arc<RenderImage>>,
    path: PathBuf,
    mime: Option<String>,
    looped: bool,
    playing: bool,
    ended: bool,
    muted: bool,
    /// Where a drag on the bar has put the video, as a share of its length.
    scrub: Option<f32>,
    /// Index into `SPEEDS`.
    speed: usize,
    /// What the skips of the current run moved, in ms (negative back), and when the last one was.
    skipped: Option<(i64, Instant)>,
    focus: FocusHandle,
    focused: bool,
    app: WeakEntity<BeeperApp>,
    _tick: Task<()>,
}

/// Starts `path` playing in a player over the window; None when GStreamer cannot.
pub(super) fn open(path: &Path, media: &Media, app: WeakEntity<BeeperApp>, cx: &mut App) -> Option<AnyView> {
    let newest = Arc::new(Mutex::new(None));
    let playbin = start(path, media.looped, newest.clone()).map_err(|e| log::warn!("video player: {e}")).ok()?;
    let player = cx.new(|cx| {
        let tick = cx.spawn(async move |this: WeakEntity<Player>, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |player, cx| player.tick(cx)).is_err() {
                    break;
                }
            }
        });
        Player {
            playbin,
            newest,
            shown: None,
            path: path.to_owned(),
            mime: media.mime.clone(),
            looped: media.looped,
            playing: true,
            ended: false,
            muted: media.looped,
            scrub: None,
            speed: 0,
            skipped: None,
            focus: cx.focus_handle(),
            focused: false,
            app,
            _tick: tick,
        }
    });
    Some(player.into())
}

fn start(path: &Path, looped: bool, newest: Arc<Mutex<Option<Picture>>>) -> anyhow::Result<gst::Element> {
    init()?;
    let uri = glib::filename_to_uri(path, None)?;
    let playbin = gst::ElementFactory::make("playbin").property("uri", uri).property("mute", looped).build()?;
    // `videoflip` turns a phone's upright video the way its orientation tag says.
    let sink = gst::parse::bin_from_description(
        "videoconvert ! videoflip video-direction=auto ! videoscale ! video/x-raw,format=BGRA,pixel-aspect-ratio=1/1 ! appsink name=sink max-buffers=1 drop=true emit-signals=true",
        true,
    )?;
    let Some(appsink) = sink.by_name("sink") else { anyhow::bail!("no appsink") };
    // A paused video shows the frame a seek lands on as its preroll, not as a sample.
    for (signal, pull) in [("new-sample", "pull-sample"), ("new-preroll", "pull-preroll")] {
        let newest = newest.clone();
        appsink.connect(signal, false, move |args| {
            if let Some(picture) = args[0].get::<gst::Element>().ok().and_then(|sink| take(&sink, pull, &[])) {
                *newest.lock().unwrap_or_else(|e| e.into_inner()) = Some(picture);
            }
            Some(gst::FlowReturn::Ok.to_value())
        });
    }
    playbin.set_property("video-sink", &sink);
    // Keeps voices at their pitch above 1x, as voice notes do; without it a faster video still plays.
    match gst::parse::bin_from_description("audioconvert ! scaletempo ! audioconvert", true) {
        Ok(tempo) => playbin.set_property("audio-filter", &tempo),
        Err(e) => log::debug!("video player: no scaletempo: {e}"),
    }
    playbin.set_state(gst::State::Playing)?;
    Ok(playbin)
}

/// The first frame of the video at `path` as JPEG, its long side at most `max`, for the thumbnail a
/// sent video carries; None for a file without a picture. Cost: one decoded frame (8 MB at 1080p)
/// and a paused pipeline, for the moment it takes; blocks up to `FRAME_WAIT`.
pub(in crate::ui) fn frame(path: &Path, max: u32) -> Option<Vec<u8>> {
    const FRAME_WAIT: u64 = 5;
    let read = || -> anyhow::Result<Option<Vec<u8>>> {
        init()?;
        let playbin = gst::ElementFactory::make("playbin").property("uri", glib::filename_to_uri(path, None)?).build()?;
        let sink = gst::parse::bin_from_description("videoconvert ! videoflip video-direction=auto ! videoscale ! video/x-raw,format=BGRA,pixel-aspect-ratio=1/1 ! appsink name=sink", true)?;
        let Some(appsink) = sink.by_name("sink") else { anyhow::bail!("no appsink") };
        playbin.set_property("video-sink", &sink);
        playbin.set_property("audio-sink", gst::ElementFactory::make("fakesink").build()?);
        playbin.set_state(gst::State::Paused)?;
        let picture = take(&appsink, "try-pull-preroll", &[&gst::ClockTime::from_seconds(FRAME_WAIT).nseconds()]);
        let _ = playbin.set_state(gst::State::Null);
        let Some(Picture { width, height, bytes }) = picture else { return Ok(None) };
        let rgb = bytes.as_chunks::<4>().0.iter().flat_map(|&[b, g, r, _]| [r, g, b]).collect();
        let mut still = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| anyhow::anyhow!("short frame"))?;
        if width.max(height) > max {
            let scale = max as f32 / width.max(height) as f32;
            let (w, h) = (((width as f32 * scale).round() as u32).max(1), ((height as f32 * scale).round() as u32).max(1));
            still = image::imageops::resize(&still, w, h, image::imageops::FilterType::Triangle);
        }
        Ok(Some(crate::backend::caption::jpeg(&still)?))
    };
    read().map_err(|e| log::warn!("video frame: {e:#}")).ok().flatten()
}

/// GStreamer, with NVIDIA's AV1 decoder set aside: it stamps frames at the stream's nominal rate,
/// so a screen recording of variable rate played too fast (GStreamer 1.28; its H.264 and VP9
/// decoders keep the file's times). `dav1ddec` or `av1dec` decode AV1 instead, on the CPU.
fn init() -> anyhow::Result<()> {
    static AV1: std::sync::Once = std::sync::Once::new();
    gst::init()?;
    AV1.call_once(|| {
        if let Some(nvidia) = gst::Registry::get().lookup_feature("nvav1dec") {
            nvidia.set_rank(gst::Rank::NONE);
        }
    });
    Ok(())
}

/// The frame the sink holds (`pull` names which, `args` its timeout if any), copied out of GStreamer's buffer.
fn take(sink: &gst::Element, pull: &str, args: &[&dyn ToValue]) -> Option<Picture> {
    let sample = sink.emit_by_name::<Option<gst::Sample>>(pull, args)?;
    let caps = sample.caps()?.structure(0)?;
    let (width, height) = (u32::try_from(caps.get::<i32>("width").ok()?).ok()?, u32::try_from(caps.get::<i32>("height").ok()?).ok()?);
    let map = sample.buffer()?.map_readable().ok()?;
    // BGRA rows need no padding, so a frame is exactly its pixels.
    let size = width as usize * height as usize * 4;
    (map.len() >= size).then(|| Picture { width, height, bytes: map[..size].to_vec() })
}

impl Player {
    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.newest.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        if let Some(msg) = self.playbin.bus().and_then(|bus| bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])) {
            match msg.view() {
                gst::MessageView::Error(e) => {
                    log::warn!("video player: {}", e.error());
                    // A codec GStreamer lacks: the system's app may have it.
                    let (path, mime) = (self.path.clone(), self.mime.clone());
                    self.close(None, cx);
                    crate::open_file(cx, &path, mime.as_deref());
                    return;
                }
                _ if self.looped => self.seek(0., true),
                _ => {
                    (self.playing, self.ended) = (false, true);
                    let _ = self.playbin.set_state(gst::State::Paused);
                }
            }
            changed = true;
        }
        if self.skipped.is_some_and(|(_, at)| at.elapsed() >= SKIP_SHOWN) {
            self.skipped = None;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.ended {
            self.seek(0., true);
        }
        self.playing = !self.playing;
        let _ = self.playbin.set_state(if self.playing { gst::State::Playing } else { gst::State::Paused });
        cx.notify();
    }

    fn mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.playbin.set_property("mute", self.muted);
        cx.notify();
    }

    fn length(&self) -> Option<gst::ClockTime> {
        self.playbin.query_duration::<gst::ClockTime>().filter(|l| !l.is_zero())
    }

    fn at(&self) -> gst::ClockTime {
        if self.ended {
            return self.length().unwrap_or_default();
        }
        self.playbin.query_position::<gst::ClockTime>().unwrap_or_default()
    }

    /// Goes to `share` of the length: to that very frame when `exact`, else to the nearest key
    /// frame, which is quick enough to follow a drag. Every seek carries the speed, which a seek
    /// without one would set back to 1x.
    fn seek(&mut self, share: f32, exact: bool) {
        let Some(length) = self.length() else { return };
        let to = gst::ClockTime::from_nseconds((length.nseconds() as f64 * f64::from(share.clamp(0., 1.))) as u64);
        self.ended = false;
        let how = if exact { gst::SeekFlags::ACCURATE } else { gst::SeekFlags::KEY_UNIT };
        let rate = f64::from(SPEEDS[self.speed].0);
        if let Err(e) = self.playbin.seek(rate, gst::SeekFlags::FLUSH | how, gst::SeekType::Set, to, gst::SeekType::None, gst::ClockTime::NONE) {
            log::warn!("video seek: {e}");
        }
    }

    /// Moves `by` back or ahead and names the run's total over the side it went to.
    fn skip(&mut self, by: Duration, forward: bool, cx: &mut Context<Self>) {
        let Some(length) = self.length() else { return };
        let (from, by) = (self.at(), gst::ClockTime::from_nseconds(by.as_nanos() as u64));
        let to = if forward { from.saturating_add(by).min(length) } else { from.saturating_sub(by) };
        self.seek(to.nseconds() as f32 / length.nseconds() as f32, true);
        let moved = to.mseconds() as i64 - from.mseconds() as i64;
        let run = self.skipped.filter(|(total, _)| (*total > 0) == forward).map_or(0, |(total, _)| total);
        self.skipped = (run + moved != 0).then(|| (run + moved, Instant::now()));
        cx.notify();
    }

    fn next_speed(&mut self, cx: &mut Context<Self>) {
        self.speed = (self.speed + 1) % SPEEDS.len();
        // A video at its end starts over at the new speed when played.
        if !self.ended
            && let Some(length) = self.length()
        {
            self.seek(self.at().nseconds() as f32 / length.nseconds() as f32, true);
        }
        cx.notify();
    }

    /// A click on the picture plays or pauses; a second quick one on either side takes that back
    /// and skips instead, and each one after skips again.
    fn clicked(&mut self, e: &ClickEvent, window: &Window, cx: &mut Context<Self>) {
        let (x, width) = (e.position().x, window.viewport_size().width);
        let side = if x < width * 0.35 { Some(false) } else if x > width * 0.65 { Some(true) } else { None };
        match (side, e.click_count()) {
            (Some(forward), count) if count >= 2 && !self.looped => {
                if count == 2 {
                    self.toggle(cx);
                }
                self.skip(CLICK_SKIP, forward, cx);
            }
            _ => self.toggle(cx),
        }
    }

    /// `window` is the one being drawn when a click or key closes; a timer has none leased, and
    /// then every window is reached without it.
    fn close(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let _ = self.playbin.set_state(gst::State::Null);
        if let Some(shown) = self.shown.take() {
            cx.drop_image(shown, window);
        }
        let _ = self.app.update(cx, |app, cx| {
            app.video = None;
            cx.notify();
        });
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "escape" => self.close(Some(window), cx),
            "space" | "k" => self.toggle(cx),
            "m" => self.mute(cx),
            "left" => self.skip(SKIP, false, cx),
            "right" => self.skip(SKIP, true, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

/// A round button on the dark player, white in either theme.
fn button(id: &'static str, icon: IconName, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(40.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .text_color(white())
        .hover(|el| el.bg(white().opacity(0.12)))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label).build(window, cx))
        .child(Icon::new(icon).size_5())
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picture) = self.newest.lock().unwrap_or_else(|e| e.into_inner()).take()
            && let Some(pixels) = image::RgbaImage::from_raw(picture.width, picture.height, picture.bytes)
        {
            let image = Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]));
            if let Some(old) = self.shown.replace(image) {
                cx.drop_image(old, Some(window));
            }
        }
        if !self.focused {
            self.focused = true;
            window.focus(&self.focus, cx);
        }
        let (at, total) = (self.at(), self.length());
        let share = self.scrub.unwrap_or_else(|| total.map_or(0., |l| at.nseconds() as f32 / l.nseconds() as f32)).clamp(0., 1.);
        let time = |t: gst::ClockTime| length(Duration::from_nanos(t.nseconds()));
        let weak = cx.entity().downgrade();

        let bar = div()
            .flex_1()
            .h(px(24.))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                div()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(white().opacity(0.3))
                    .child(div().h_full().w(relative(share)).rounded_full().bg(white())),
            )
            .child(
                canvas(
                    |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |bounds, hitbox, window, _| {
                        let share = move |x: Pixels| f32::from(x - bounds.left()) / f32::from(bounds.size.width);
                        window.on_mouse_event({
                            let weak = weak.clone();
                            move |e: &MouseDownEvent, phase, window, cx| {
                                if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                    cx.stop_propagation();
                                    let _ = weak.update(cx, |player, cx| {
                                        player.scrub = Some(share(e.position.x).clamp(0., 1.));
                                        player.seek(share(e.position.x), true);
                                        cx.notify();
                                    });
                                }
                            }
                        });
                        window.on_mouse_event({
                            let weak = weak.clone();
                            move |e: &MouseMoveEvent, phase, _, cx| {
                                if phase.bubble() && e.pressed_button == Some(MouseButton::Left) {
                                    let _ = weak.update(cx, |player, cx| {
                                        if player.scrub.is_some() {
                                            player.scrub = Some(share(e.position.x).clamp(0., 1.));
                                            player.seek(share(e.position.x), false);
                                            cx.notify();
                                        }
                                    });
                                }
                            }
                        });
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase.bubble() {
                                let _ = weak.update(cx, |player, cx| {
                                    // The drag went by key frames; it ends on the frame let go at.
                                    if let Some(at) = player.scrub.take() {
                                        player.seek(at, true);
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .relative();

        // Occluding: a click on them must not also reach the picture, which pauses on a click.
        let controls = h_flex()
            .id("video-controls")
            .occlude()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px(px(12.))
            .py(px(10.))
            .gap(px(12.))
            .bg(black().opacity(0.6))
            .text_color(white())
            .text_size(fs(13.))
            .child(button("video-play", if self.playing { IconName::Pause } else { IconName::Play }, if self.playing { t!("Pause") } else { t!("Play") }).on_click(cx.listener(|player, _, _, cx| player.toggle(cx))))
            .child(div().flex_none().child(time(at)))
            .child(bar)
            .child(div().flex_none().child(total.map_or_else(String::new, time)))
            .when(!self.looped, |el| {
                el.child(
                    div()
                        .id("video-speed")
                        .flex_none()
                        .min_w(px(40.))
                        .h(px(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .font_features(crate::ui::tabular())
                        .hover(|el| el.bg(white().opacity(0.12)))
                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Playback speed")).build(window, cx))
                        .on_click(cx.listener(|player, _, _, cx| player.next_speed(cx)))
                        .child(SPEEDS[self.speed].1),
                )
            })
            .child(
                button("video-mute", if self.muted { IconName::VolumeX } else { IconName::Volume2 }, if self.muted { t!("Unmute") } else { t!("Mute") })
                    .on_click(cx.listener(|player, _, _, cx| player.mute(cx))),
            );

        div()
            .id("video-player")
            .absolute()
            .inset_0()
            .bg(black())
            .occlude()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|player, e: &KeyDownEvent, window, cx| player.on_key(e, window, cx)))
            .child(
                div()
                    .id("video-picture")
                    .size_full()
                    .on_click(cx.listener(|player, e: &ClickEvent, window, cx| player.clicked(e, window, cx)))
                    .children(self.shown.clone().map(|shown| img(ImageSource::Render(shown)).size_full().object_fit(ObjectFit::Contain))),
            )
            .child(div().absolute().top(px(12.)).right(px(12.)).occlude().child(button("video-close", IconName::X, t!("Close")).on_click(cx.listener(|player, _, window, cx| player.close(Some(window), cx)))))
            .children(self.skipped.map(|(ms, _)| {
                let label = format!("{}{} s", if ms < 0 { "\u{2212}" } else { "+" }, (ms.unsigned_abs() + 500) / 1000);
                let pill = div().px(px(14.)).py(px(8.)).rounded_full().bg(black().opacity(0.6)).text_color(white()).text_size(fs(15.)).font_features(crate::ui::tabular()).child(label);
                let row = h_flex().absolute().inset_0().items_center().px(px(40.));
                if ms < 0 { row.justify_start() } else { row.justify_end() }.child(pill)
            }))
            .child(controls)
    }
}

#[cfg(test)]
mod tests {
    use super::gst;
    use gstreamer::prelude::*;

    #[test]
    fn a_video_gives_its_first_frame_as_a_fitted_jpeg() {
        let dir = std::env::temp_dir().join(format!("unisend-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("test.mkv");
        // Raw frames in Matroska: no encoder plugin needed.
        let launch = format!("videotestsrc num-buffers=3 ! video/x-raw,width=320,height=180,format=I420,framerate=10/1 ! matroskamux ! filesink location={}", video.display());
        gst::init().unwrap();
        let pipeline = gst::parse::launch(&launch).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let bus = pipeline.bus().unwrap();
        bus.timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Eos, gst::MessageType::Error]).unwrap();
        pipeline.set_state(gst::State::Null).unwrap();
        let jpeg = super::frame(&video, 160).expect("a frame");
        let still = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((still.width(), still.height()), (160, 90));
        assert!(super::frame(&dir.join("missing.mkv"), 160).is_none());
        assert!(gst::Registry::get().lookup_feature("nvav1dec").is_none_or(|f| f.rank() == gst::Rank::NONE), "NVIDIA's AV1 decoder set aside");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
