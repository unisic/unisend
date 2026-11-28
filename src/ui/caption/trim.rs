//! Trimming a video before it leaves, as Signal's phone apps do: a start and an end chosen on a
//! bar under the video's still, then the stretch copied to a new MP4 without re-encoding, so it
//! begins at the key frame at or before the chosen start. Android copies with `MediaExtractor` and
//! `MediaMuxer` (`dev.gpui.beeper.VideoTrim`), Linux with GStreamer (`qtdemux` and `mp4mux`, from
//! the system's plugins; without them there is no trim button). Windows and macOS have none: the
//! app links no media framework there. Only MP4 and MOV are offered: the send refuses the rest.
//!
//! The copy lands in the account's `pasted` folder, which the send deletes, and the item sends it
//! in place of the original; trimming again starts from the original. Cost: one pass over the
//! kept stretch on a background thread, one sample in memory at a time (at most 32 MB on Android,
//! GStreamer's queues on Linux). The length is read from the file's `mvhd` (the `moov` box, read
//! whole, at most `MAX_MOOV`).

use std::io::{Read, Seek, SeekFrom};

use super::*;

/// The shortest stretch kept, in seconds.
const MIN_LENGTH: f64 = 1.;
/// A knob is taken this close to the finger (px).
const GRAB: f32 = 44.;
const MAX_MOOV: u64 = 64 << 20;

pub(super) struct Trimmer {
    entry: u64,
    serial: u64,
    source: PathBuf,
    /// Length in seconds.
    length: f64,
    /// The stretch kept, in seconds.
    range: [f64; 2],
    /// The knob held: 0 the start, 1 the end.
    held: Option<usize>,
    saving: bool,
    bar: Rc<Cell<Bounds<Pixels>>>,
}

/// A video this platform can trim.
pub(super) fn trimmable(path: &Path) -> bool {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    matches!(mime.essence_str(), "video/mp4" | "video/quicktime") && available()
}

#[cfg(target_os = "android")]
fn available() -> bool {
    true
}

#[cfg(target_os = "linux")]
fn available() -> bool {
    use gstreamer as gst;
    static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READY.get_or_init(|| gst::init().is_ok() && ["filesrc", "qtdemux", "queue", "mp4mux", "filesink"].iter().all(|name| gst::ElementFactory::find(name).is_some()))
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn available() -> bool {
    false
}

/// The movie's length in seconds, from `moov/mvhd`.
fn length(path: &Path) -> anyhow::Result<f64> {
    let mut file = std::fs::File::open(path)?;
    let end = file.metadata()?.len();
    let mut at = 0;
    while at + 8 <= end {
        let mut head = [0u8; 16];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut head[..8])?;
        let kind = [head[4], head[5], head[6], head[7]];
        let (size, skip) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            0 => (end - at, 8),
            1 => {
                file.read_exact(&mut head[8..])?;
                (u64::from_be_bytes(head[8..16].try_into()?), 16)
            }
            n => (n as u64, 8),
        };
        anyhow::ensure!(size >= skip, "bad box size");
        if &kind == b"moov" {
            anyhow::ensure!(size <= MAX_MOOV, "moov too large");
            let mut moov = vec![0u8; (size - skip) as usize];
            file.read_exact(&mut moov)?;
            return mvhd(&moov);
        }
        at += size;
    }
    anyhow::bail!("no moov")
}

fn mvhd(moov: &[u8]) -> anyhow::Result<f64> {
    let mut at = 0;
    while at + 8 <= moov.len() {
        let size = u32::from_be_bytes(moov[at..at + 4].try_into()?) as usize;
        anyhow::ensure!(size >= 8 && at + size <= moov.len(), "bad box size");
        if &moov[at + 4..at + 8] == b"mvhd" {
            let b = &moov[at + 8..at + size];
            let (scale, duration) = match b.first() {
                Some(1) if b.len() >= 32 => (u32::from_be_bytes(b[20..24].try_into()?), u64::from_be_bytes(b[24..32].try_into()?)),
                Some(0) if b.len() >= 20 => (u32::from_be_bytes(b[12..16].try_into()?), u32::from_be_bytes(b[16..20].try_into()?) as u64),
                _ => anyhow::bail!("bad mvhd"),
            };
            anyhow::ensure!(scale > 0, "no time scale");
            return Ok(duration as f64 / scale as f64);
        }
        at += size;
    }
    anyhow::bail!("no mvhd")
}

#[cfg(target_os = "android")]
fn cut(path: &Path, [from, to]: [f64; 2], out: &Path) -> anyhow::Result<()> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    let done = helpers::with_env(|env| {
        let class = helpers::find_app_class(env, "dev.gpui.beeper.VideoTrim")?;
        let path = env.new_string(path.to_string_lossy()).map_err(|e| e.to_string())?;
        let out = env.new_string(out.to_string_lossy()).map_err(|e| e.to_string())?;
        let us = |s: f64| JValue::Long((s * 1e6) as i64);
        env.call_static_method(&class, jni::jni_str!("trim"), jni::jni_sig!("(Ljava/lang/String;JJLjava/lang/String;)Z"), &[JValue::Object(&path), us(from), us(to), JValue::Object(&out)])
            .and_then(|v| v.z())
            .map_err(|e| crate::java_error(env, e))
    });
    match done {
        Ok(true) => Ok(()),
        Ok(false) => anyhow::bail!("not copied"),
        Err(e) => anyhow::bail!("{e}"),
    }
}

/// The demuxer's pads are held until its seek is done, so the muxer only ever sees the stretch:
/// a seek through the muxer would flush a file it has begun to write.
#[cfg(target_os = "linux")]
fn cut(path: &Path, [from, to]: [f64; 2], out: &Path) -> anyhow::Result<()> {
    use gstreamer as gst;
    use gst::prelude::*;

    let make = |name: &str| gst::ElementFactory::make(name).build();
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("filesrc").property("location", path.to_string_lossy().as_ref()).build()?;
    let demux = make("qtdemux")?;
    let mux = make("mp4mux")?;
    let sink = gst::ElementFactory::make("filesink").property("location", out.to_string_lossy().as_ref()).build()?;
    pipeline.add_many([&src, &demux, &mux, &sink])?;
    src.link(&demux)?;
    mux.link(&sink)?;
    let held: Arc<std::sync::Mutex<Vec<(gst::Pad, gst::PadProbeId)>>> = Arc::default();
    let (bin, kept) = (pipeline.downgrade(), held.clone());
    demux.connect_pad_added(move |_, pad| {
        let name = pad.name();
        // Subtitles and timed metadata stay behind.
        if !(name.starts_with("video_") || name.starts_with("audio_")) {
            return;
        }
        let (Some(bin), Ok(queue)) = (bin.upgrade(), gst::ElementFactory::make("queue").build()) else { return };
        let linked = bin.add(&queue).is_ok()
            && queue.sync_state_with_parent().is_ok()
            && queue.static_pad("sink").is_some_and(|sink| pad.link(&sink).is_ok())
            && queue.link(&mux).is_ok();
        if !linked {
            log::warn!("video trim: a stream was not linked");
        }
        if let Some(id) = pad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_, _| gst::PadProbeReturn::Ok) {
            kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((pad.clone(), id));
        }
    });
    let (ready, pads) = std::sync::mpsc::sync_channel(1);
    demux.connect_no_more_pads(move |_| {
        let _ = ready.try_send(());
    });
    let run = || -> anyhow::Result<()> {
        let bus = pipeline.bus().ok_or_else(|| anyhow::anyhow!("no bus"))?;
        pipeline.set_state(gst::State::Paused)?;
        // A file the demuxer cannot read ends in an error before any stream shows.
        let mut tries = 0;
        while pads.recv_timeout(Duration::from_millis(100)).is_err() {
            if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
                && let gst::MessageView::Error(e) = msg.view()
            {
                anyhow::bail!("{}", e.error());
            }
            tries += 1;
            anyhow::ensure!(tries < 100, "no streams");
        }
        let at = |s: f64| gst::ClockTime::from_useconds((s * 1e6) as u64);
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_BEFORE;
        demux.seek(1., flags, gst::SeekType::Set, at(from), gst::SeekType::Set, at(to))?;
        for (pad, id) in held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain(..) {
            pad.remove_probe(id);
        }
        pipeline.set_state(gst::State::Playing)?;
        let end = bus.timed_pop_filtered(gst::ClockTime::from_seconds(600), &[gst::MessageType::Eos, gst::MessageType::Error]);
        match end.as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Eos(_)) => Ok(()),
            Some(gst::MessageView::Error(e)) => anyhow::bail!("{}", e.error()),
            _ => anyhow::bail!("timed out"),
        }
    };
    let done = run();
    let _ = pipeline.set_state(gst::State::Null);
    if done.is_err() {
        let _ = std::fs::remove_file(out);
    }
    done
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn cut(_: &Path, _: [f64; 2], _: &Path) -> anyhow::Result<()> {
    anyhow::bail!("no trim on this platform")
}

/// `seconds` as m:ss.
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A knob moved to `at` seconds, kept inside the video and `MIN_LENGTH` from the other.
fn moved([from, to]: [f64; 2], knob: usize, at: f64, length: f64) -> [f64; 2] {
    let min = MIN_LENGTH.min(length);
    if knob == 0 { [at.clamp(0., (to - min).max(0.)), to] } else { [from, at.clamp((from + min).min(length), length)] }
}

impl BeeperApp {
    pub(super) fn open_trim(&mut self, ix: usize, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        let Some(entry) = p.entries.get(ix).filter(|e| e.video && p.trimmer.is_none() && p.editor.is_none()) else { return };
        let (source, range) = match &entry.cut {
            Some((source, range)) => (source.clone(), Some(*range)),
            None => (entry.path.clone(), None),
        };
        let entry = entry.id;
        p.next_id += 1;
        let serial = p.next_id;
        let read = source.clone();
        cx.spawn(async move |this, cx| {
            let length = cx.background_spawn(async move { length(&read) }).await;
            let _ = this.update(cx, |this, cx| {
                match length {
                    Ok(length) if length > 0. => {
                        this.preview.trimmer = Some(Trimmer {
                            entry,
                            serial,
                            source,
                            length,
                            range: range.unwrap_or([0., length]),
                            held: None,
                            saving: false,
                            bar: Rc::default(),
                        });
                    }
                    Ok(_) | Err(_) => {
                        if let Err(e) = length {
                            log::warn!("video length: {e:#}");
                        }
                        this.error = Some(t!("Could not read this video").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trim_pointer(&mut self, phase: TouchPhase, at: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(tr) = self.preview.trimmer.as_mut().filter(|t| !t.saving) else { return false };
        let bar = tr.bar.get();
        let w = f32::from(bar.size.width);
        if w < 1. {
            return false;
        }
        let x = f32::from(at.x - bar.origin.x);
        let seconds = (x / w).clamp(0., 1.) as f64 * tr.length;
        match phase {
            TouchPhase::Started => {
                let near = |s: f64| ((s / tr.length) as f32 * w - x).abs();
                let (a, b) = (near(tr.range[0]), near(tr.range[1]));
                tr.held = [(0, a), (1, b)].into_iter().filter(|&(_, d)| d < GRAB).min_by(|p, q| p.1.total_cmp(&q.1)).map(|(knob, _)| knob);
            }
            TouchPhase::Moved => {}
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if tr.held.take().is_none() {
                    return false;
                }
                cx.notify();
                return true;
            }
        }
        let Some(knob) = tr.held else { return false };
        tr.range = moved(tr.range, knob, seconds, tr.length);
        cx.notify();
        true
    }

    fn close_trim(&mut self, cx: &mut Context<Self>) {
        self.preview.trimmer = None;
        cx.notify();
    }

    fn finish_trim(&mut self, cx: &mut Context<Self>) {
        let Some(tr) = self.preview.trimmer.as_mut().filter(|t| !t.saving) else { return };
        let (entry, serial, range) = (tr.entry, tr.serial, tr.range);
        // Within a frame of the ends is the whole video.
        if range[0] < 0.05 && range[1] > tr.length - 0.05 {
            self.set_cut(entry, None, cx);
            return self.close_trim(cx);
        }
        tr.saving = true;
        let source = tr.source.clone();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let folder = self.account_dir().join(crate::backend::PASTED).join(format!("{stamp}-trim-{serial}"));
        cx.spawn(async move |this, cx| {
            let made = {
                let source = source.clone();
                cx.background_spawn(async move {
                    std::fs::create_dir_all(&folder)?;
                    let out = folder.join("video.mp4");
                    let done = cut(&source, range, &out);
                    if done.is_err() {
                        let _ = std::fs::remove_dir_all(&folder);
                    }
                    done.map(|()| out)
                })
                .await
            };
            let _ = this.update(cx, |this, cx| {
                let open = this.preview.trimmer.as_ref().is_some_and(|t| t.serial == serial);
                match made {
                    Ok(path) if open => {
                        this.set_cut(entry, Some((path, source, range)), cx);
                        this.close_trim(cx);
                    }
                    Ok(path) => this.drop_copy(&path, cx),
                    Err(e) => {
                        log::warn!("video trim: {e:#}");
                        this.error = Some(t!("Could not trim this video").into());
                        if let Some(tr) = this.preview.trimmer.as_mut().filter(|_| open) {
                            tr.saving = false;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Puts a trimmed copy (its path, the original's, the stretch) in the item's place, or the
    /// original back; the copy it had goes.
    fn set_cut(&mut self, entry: u64, made: Option<(PathBuf, PathBuf, [f64; 2])>, cx: &mut Context<Self>) {
        let Some(e) = self.preview.entries.iter_mut().find(|e| e.id == entry) else {
            if let Some((path, _, _)) = made {
                self.drop_copy(&path, cx);
            }
            return;
        };
        let previous = e.cut.take();
        let old = previous.as_ref().map(|_| e.path.clone());
        match (made, previous) {
            (Some((path, source, range)), _) => {
                e.path = path;
                e.cut = Some((source, range));
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

    pub(super) fn render_trim(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tr = self.preview.trimmer.as_ref()?;
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let (white, black) = (gpui_kit::white(), gpui_kit::black());
        let still = self.preview.entries.iter().find(|e| e.id == tr.entry).and_then(|e| e.big.clone().or_else(|| e.strip.clone()));

        let bar = h_flex()
            .h(px(TAP))
            .flex_none()
            .px(px(GUTTER - 10.))
            .justify_between()
            .text_color(white)
            .child(round_button("trim-close", IconName::X, t!("Close"), cx).on_click(cx.listener(|this, _, _, cx| this.close_trim(cx))))
            .child(
                div()
                    .id("trim-done")
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
                    .when(tr.saving, |el| el.opacity(0.6))
                    .on_click(cx.listener(|this, _, _, cx| this.finish_trim(cx)))
                    .child(if tr.saving { t!("Saving…") } else { t!("Done") }),
            );

        let stage = div().flex_1().min_h_0().p(px(GUTTER)).flex().items_center().justify_center().map(|el| match still {
            Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Contain)),
            None => el.text_color(p.faint).child(Icon::new(IconName::Video).size_8()),
        });

        let [from, to] = [(tr.range[0] / tr.length) as f32, (tr.range[1] / tr.length) as f32];
        let cell = tr.bar.clone();
        let weak = cx.entity().downgrade();
        let track = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                cell.set(bounds);
                let w = bounds.size.width;
                let x = |f: f32| bounds.origin.x + w * f;
                let (top, bottom) = (bounds.origin.y + px(6.), bounds.origin.y + bounds.size.height - px(6.));
                let line = Bounds::from_corners(point(bounds.origin.x, top), point(bounds.origin.x + w, bottom));
                window.paint_quad(fill(line, white.opacity(0.15)).corner_radii(px(8.)));
                let kept = Bounds::from_corners(point(x(from), top), point(x(to), bottom));
                window.paint_quad(fill(kept, white.opacity(0.3)));
                window.paint_quad(outline(kept, white, BorderStyle::Solid));
                for f in [from, to] {
                    let knob = Bounds::from_corners(point(x(f) - px(4.), bounds.origin.y), point(x(f) + px(4.), bounds.origin.y + bounds.size.height));
                    window.paint_quad(fill(knob, white).corner_radii(px(4.)));
                }
                edit::listen(weak, bounds, window, Self::trim_pointer);
            },
        )
        .h(px(56.))
        .w_full();
        let times = h_flex()
            .justify_between()
            .text_size(fs(14.))
            .text_color(p.muted)
            .child(clock(tr.range[0]))
            .child(div().text_color(white).child(clock(tr.range[1] - tr.range[0])))
            .child(clock(tr.range[1]));

        Some(
            v_flex()
                .id("trim")
                .absolute()
                .inset_0()
                .pt(px(top))
                .pb(px(bottom))
                .bg(black)
                .occlude()
                .child(bar)
                .child(stage)
                .child(v_flex().flex_none().px(px(GUTTER + 4.)).pb(px(16.)).gap(px(8.)).child(track).child(times))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_LENGTH, clock, length, moved, mvhd};

    fn mvhd_box(version: u8, scale: u32, duration: u64) -> Vec<u8> {
        let mut body = vec![version, 0, 0, 0];
        if version == 1 {
            body.extend_from_slice(&[0; 16]);
            body.extend_from_slice(&scale.to_be_bytes());
            body.extend_from_slice(&duration.to_be_bytes());
        } else {
            body.extend_from_slice(&[0; 8]);
            body.extend_from_slice(&scale.to_be_bytes());
            body.extend_from_slice(&(duration as u32).to_be_bytes());
        }
        body.extend_from_slice(&[0; 80]);
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(b"mvhd");
        b.extend(body);
        b
    }

    #[test]
    fn the_length_comes_from_mvhd_wherever_moov_is() {
        let mut moov = vec![0, 0, 0, 8, b'f', b'r', b'e', b'e'];
        moov.extend(mvhd_box(0, 1000, 12_500));
        assert_eq!(mvhd(&moov).unwrap(), 12.5);
        assert_eq!(mvhd(&mvhd_box(1, 600, 600 * 90)).unwrap(), 90.);
        assert!(mvhd(&[0, 0, 0, 4, b'm', b'v', b'h', b'd']).is_err());

        // ftyp, a large mdat, then moov, as most cameras write it.
        let mut file = vec![0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 0, 0];
        file.extend_from_slice(&[0, 0, 0, 1, b'm', b'd', b'a', b't']);
        file.extend_from_slice(&(16u64 + 1000).to_be_bytes());
        file.extend(vec![0; 1000]);
        let inner = mvhd_box(0, 30, 300);
        file.extend_from_slice(&((inner.len() + 8) as u32).to_be_bytes());
        file.extend_from_slice(b"moov");
        file.extend(inner);
        let path = std::env::temp_dir().join(format!("unisend-trim-{}.mp4", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        assert_eq!(length(&path).unwrap(), 10.);
        std::fs::write(&path, &file[..file.len() - 20]).unwrap();
        assert!(length(&path).is_err());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn knobs_stay_inside_and_apart() {
        assert_eq!(moved([0., 10.], 0, 9.8, 10.), [10. - MIN_LENGTH, 10.]);
        assert_eq!(moved([2., 10.], 1, 1., 10.), [2., 2. + MIN_LENGTH]);
        assert_eq!(moved([2., 8.], 1, 12., 10.), [2., 10.]);
        assert_eq!(moved([2., 8.], 0, -3., 10.), [0., 8.]);
        // A video shorter than the shortest stretch keeps all of it.
        assert_eq!(moved([0., 0.5], 1, 0.2, 0.5), [0., 0.5]);
    }

    #[test]
    fn times_read_as_minutes_and_seconds() {
        assert_eq!(clock(0.), "0:00");
        assert_eq!(clock(65.4), "1:05");
        assert_eq!(clock(600.), "10:00");
    }

    /// Needs GStreamer's good plugins (`qtdemux`, `mp4mux`), as the app does for a trim on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_trim_keeps_the_stretch_from_the_key_frame_before_it() {
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clip.mp4");
        assert!(super::available(), "GStreamer with qtdemux and mp4mux is missing");
        // 4 s at 10 fps, a key frame every 0.5 s: 1.2 to 2.6 starts at the key frame at 1.0.
        assert!((length(&clip).unwrap() - 4.128).abs() < 0.01);
        let out = std::env::temp_dir().join(format!("unisend-cut-{}.mp4", std::process::id()));
        super::cut(&clip, [1.2, 2.6], &out).unwrap();
        let kept = length(&out).unwrap();
        assert!((1.5..=1.8).contains(&kept), "{kept}");
        std::fs::remove_file(&out).ok();
        // A file that is not a video leaves nothing behind.
        let bad = std::env::temp_dir().join(format!("unisend-cut-bad-{}.mp4", std::process::id()));
        std::fs::write(&bad, b"not a video").unwrap();
        assert!(super::cut(&bad, [0., 1.], &out).is_err());
        assert!(!out.exists());
        std::fs::remove_file(bad).ok();
    }
}
