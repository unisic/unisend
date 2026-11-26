//! Voice messages on Linux, through the system's GStreamer (base and good plugins): one player
//! (`playbin`) and one recorder (the default microphone to Ogg Opus) at a time. Every call but
//! `ready`, `access` and `wave` runs on the voice thread (`post` in voice.rs), which owns both
//! through a thread local. `wave` decodes a file on its own, for the waveform a message lacks and
//! for the gain that brings a quiet one up to full scale.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;

/// MSC3245's suggested settings, which `Voice.java` records with as well: 48 kHz mono at 24 kbit/s.
const RATE: u64 = 48_000;
const BITRATE: i32 = 24_000;
/// Recordings older than this are left from a crash and go when the next one starts.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest a state change, or the recorder's last bytes, may take.
const WAIT: gst::ClockTime = gst::ClockTime::from_seconds(3);
/// What both need; all are in GStreamer's base and good plugins.
const ELEMENTS: [&str; 10] = ["playbin", "scaletempo", "audioconvert", "audioresample", "capsfilter", "opusenc", "oggmux", "filesink", "autoaudiosrc", "autoaudiosink"];

/// The longest `wave` decodes before giving up: long audio files are not worth a waveform.
const DECODE_WAIT: gst::ClockTime = gst::ClockTime::from_seconds(10);
/// `wave`'s samples per peak at first (50 ms at 8 kHz), and the most peaks it keeps: 8 KB.
const WINDOW: usize = 400;
const MAX_PEAKS: usize = 2048;

static READY: OnceLock<bool> = OnceLock::new();

thread_local! {
    static RECORDER: RefCell<Option<Recorder>> = const { RefCell::new(None) };
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
    /// The volume for one file, kept for when it is loaded again.
    static GAIN: RefCell<Option<(PathBuf, f64)>> = const { RefCell::new(None) };
}

/// Loads GStreamer and checks its plugins; run once, off the UI thread.
pub fn init() -> bool {
    // The sound server's mixer lists the app under this name, not the binary's.
    glib::set_application_name("Unisend");
    *READY.get_or_init(|| match gst::init() {
        Ok(()) => {
            let missing: Vec<_> = ELEMENTS.into_iter().filter(|name| gst::ElementFactory::find(name).is_none()).collect();
            if !missing.is_empty() {
                log::warn!("voice: GStreamer lacks {missing:?}, audio opens in the system's player");
            }
            missing.is_empty()
        }
        Err(e) => {
            log::warn!("voice: GStreamer: {e}");
            false
        }
    })
}

/// Whether voice messages play and record inside the app; false until `init` has run.
pub fn ready() -> bool {
    READY.get().copied().unwrap_or(false)
}

/// A desktop asks no permission: the sound server or the portal decides when the recorder starts.
pub fn access() -> bool {
    ready()
}

pub fn ask() {}

pub fn start() -> Option<PathBuf> {
    discard();
    stop_player();
    let dir = dirs::cache_dir()?.join("beeper-gpui").join("voice");
    if let Err(e) = private_dir(&dir) {
        log::warn!("voice dir: {e}");
        return None;
    }
    clear_stale(&dir);
    let millis = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("voice-{millis}.ogg"));
    match Recorder::start("autoaudiosrc", &path) {
        Ok(recorder) => {
            RECORDER.with_borrow_mut(|r| *r = Some(recorder));
            Some(path)
        }
        Err(e) => {
            log::warn!("start recording: {e}");
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// The loudest sample since the last call, 0 to 32767; 0 when not recording.
pub fn level() -> i32 {
    RECORDER.with_borrow(|r| r.as_ref().map_or(0, |r| r.peak.swap(0, Ordering::Relaxed)))
}

/// Finishes the recording and returns its length; `None` when it holds nothing (then it is deleted).
pub fn stop() -> Option<Duration> {
    RECORDER.with_borrow_mut(Option::take)?.finish()
}

pub fn cancel() {
    discard();
}

pub fn delete(path: &Path) {
    if PLAYER.with_borrow(|p| p.as_ref().is_some_and(|p| p.path == path)) {
        stop_player();
    }
    let _ = std::fs::remove_file(path);
}

fn discard() {
    if let Some(recorder) = RECORDER.with_borrow_mut(Option::take) {
        let path = recorder.path.clone();
        drop(recorder);
        let _ = std::fs::remove_file(path);
    }
}

/// Plays `path` from `from` (a share of its length) at `speed`, loading it first unless it is the
/// one loaded. The length in ms, or `None` when it cannot be played.
pub fn play(path: &Path, from: f32, speed: f32) -> Option<u32> {
    let loaded = PLAYER.with_borrow(|p| p.as_ref().is_some_and(|p| p.path == path));
    if !loaded {
        stop_player();
        let player = Player::load(path, None).map_err(|e| log::warn!("load voice message: {e}")).ok()?;
        PLAYER.with_borrow_mut(|p| *p = Some(player));
    }
    let played = PLAYER.with_borrow_mut(|p| p.as_mut().map(|p| p.play(from, speed)));
    match played {
        Some(Ok(length)) => Some(length),
        Some(Err(e)) => {
            log::warn!("play voice message: {e}");
            stop_player();
            None
        }
        None => None,
    }
}

pub fn pause() {
    PLAYER.with_borrow(|p| p.as_ref().map(Player::pause));
}

/// Where playback is, in ms; `None` once it has reached the end or nothing is loaded.
pub fn position() -> Option<u32> {
    PLAYER.with_borrow_mut(|p| p.as_mut()?.position())
}

pub fn seek(ms: u32) {
    PLAYER.with_borrow_mut(|p| p.as_mut().map(|p| p.seek(ms)));
}

/// Changes the speed of what is playing; a paused player takes it on its next `play`.
pub fn speed(speed: f32) {
    PLAYER.with_borrow_mut(|p| p.as_mut().map(|p| p.speed(speed)));
}

/// Sets the volume `path` plays at (1 is as recorded), now if it is loaded, else when it is.
pub fn gain(path: &Path, gain: f32) {
    let gain = f64::from(gain);
    PLAYER.with_borrow(|p| p.as_ref().filter(|p| p.path == path).map(|p| p.playbin.set_property("volume", gain)));
    GAIN.with_borrow_mut(|g| *g = Some((path.to_owned(), gain)));
}

/// The peaks of `path`, 0 to 1, at most `MAX_PEAKS`; `None` when it cannot be decoded in time.
pub fn wave(path: &Path) -> Option<Vec<f32>> {
    decode_peaks(path).map_err(|e| log::warn!("voice waveform: {e}")).ok()
}

fn decode_peaks(path: &Path) -> anyhow::Result<Vec<f32>> {
    let location = path.to_str().ok_or_else(|| anyhow::anyhow!("path is not UTF-8"))?;
    let pipeline = gst::parse::launch(
        "filesrc name=file ! decodebin ! audioconvert ! audioresample \
         ! capsfilter name=pcm caps=audio/x-raw,format=S16LE,rate=8000,channels=1 ! fakesink sync=false",
    )?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
    let named = |name| pipeline.by_name(name).ok_or_else(|| anyhow::anyhow!("no {name}"));
    named("file")?.set_property("location", location);
    let peaks = Arc::new(Mutex::new(Peaks::default()));
    let sink = peaks.clone();
    named("pcm")?.static_pad("src").ok_or_else(|| anyhow::anyhow!("no pcm pad"))?.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let (Some(map), Ok(mut peaks)) = (info.buffer().and_then(|b| b.map_readable().ok()), sink.lock()) {
            peaks.push(&map);
        }
        gst::PadProbeReturn::Ok
    });
    pipeline.set_state(gst::State::Playing)?;
    let ended = pipeline.bus().and_then(|bus| bus.timed_pop_filtered(DECODE_WAIT, &[gst::MessageType::Eos, gst::MessageType::Error]));
    shut(&pipeline);
    let Some(msg) = ended else { anyhow::bail!("decoding took too long") };
    if let gst::MessageView::Error(e) = msg.view() {
        anyhow::bail!("{} ({:?})", e.error(), e.debug());
    }
    let peaks = std::mem::take(&mut *peaks.lock().map_err(|_| anyhow::anyhow!("peaks poisoned"))?);
    Ok(peaks.finish())
}

/// The loudest sample of each stretch of `window` samples; stretches double when `MAX_PEAKS` are full.
#[derive(Default)]
struct Peaks {
    levels: Vec<f32>,
    window: usize,
    peak: i32,
    count: usize,
}

impl Peaks {
    fn push(&mut self, pcm: &[u8]) {
        for &s in pcm.as_chunks::<2>().0 {
            self.peak = self.peak.max(i32::from(i16::from_le_bytes(s)).abs());
            self.count += 1;
            if self.count >= self.window.max(WINDOW) {
                self.close();
            }
        }
    }

    fn close(&mut self) {
        self.levels.push(self.peak.min(32767) as f32 / 32767.);
        (self.peak, self.count) = (0, 0);
        if self.levels.len() >= MAX_PEAKS {
            self.levels = self.levels.chunks(2).map(|p| p.iter().copied().fold(0., f32::max)).collect();
            self.window = self.window.max(WINDOW) * 2;
        }
    }

    fn finish(mut self) -> Vec<f32> {
        if self.count > 0 {
            self.close();
        }
        self.levels
    }
}

pub fn stop_player() {
    PLAYER.with_borrow_mut(|p| *p = None);
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn clear_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The peak of 16-bit little-endian samples, as Android's `getMaxAmplitude` gives it: 0 to 32767.
fn peak(pcm: &[u8]) -> i32 {
    pcm.as_chunks::<2>().0.iter().map(|&s| i32::from(i16::from_le_bytes(s)).abs()).max().unwrap_or(0).min(32767)
}

/// Pipelines own streaming threads: they stop when the pipeline is set to `Null`.
fn shut(element: &impl IsA<gst::Element>) {
    let _ = element.set_state(gst::State::Null);
}

/// The bus's first error, as text.
fn error(bus: &gst::Bus) -> Option<String> {
    let msg = bus.pop_filtered(&[gst::MessageType::Error])?;
    match msg.view() {
        gst::MessageView::Error(e) => Some(format!("{} ({:?})", e.error(), e.debug())),
        _ => None,
    }
}

struct Recorder {
    pipeline: gst::Pipeline,
    path: PathBuf,
    peak: Arc<AtomicI32>,
    /// Samples handed to the encoder: the recording's exact length.
    samples: Arc<AtomicU64>,
}

impl Recorder {
    /// `source` is a GStreamer source description: the microphone, or a test tone.
    fn start(source: &str, path: &Path) -> anyhow::Result<Self> {
        let location = path.to_str().ok_or_else(|| anyhow::anyhow!("path is not UTF-8"))?;
        let pipeline = gst::parse::launch(&format!(
            "{source} ! audioconvert ! audioresample ! capsfilter name=pcm caps=audio/x-raw,format=S16LE,rate={RATE},channels=1 \
             ! opusenc bitrate={BITRATE} audio-type=voice ! oggmux ! filesink name=file"
        ))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
        let recorder = Recorder { pipeline, path: path.to_owned(), peak: Arc::default(), samples: Arc::default() };
        let named = |name| recorder.pipeline.by_name(name).ok_or_else(|| anyhow::anyhow!("no {name}"));
        named("file")?.set_property("location", location);
        let (peak, samples) = (recorder.peak.clone(), recorder.samples.clone());
        named("pcm")?.static_pad("src").ok_or_else(|| anyhow::anyhow!("no pcm pad"))?.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(map) = info.buffer().and_then(|b| b.map_readable().ok()) {
                peak.fetch_max(self::peak(&map), Ordering::Relaxed);
                samples.fetch_add(map.len() as u64 / 2, Ordering::Relaxed);
            }
            gst::PadProbeReturn::Ok
        });
        recorder.pipeline.set_state(gst::State::Playing)?;
        recorder.pipeline.state(WAIT).0?;
        if let Some(e) = recorder.pipeline.bus().and_then(|bus| error(&bus)) {
            anyhow::bail!(e);
        }
        Ok(recorder)
    }

    /// Ends the stream so the file is complete, and returns its length.
    fn finish(self) -> Option<Duration> {
        self.pipeline.send_event(gst::event::Eos::new());
        let ended = self.pipeline.bus().and_then(|bus| bus.timed_pop_filtered(WAIT, &[gst::MessageType::Eos, gst::MessageType::Error]));
        let ok = ended.is_some_and(|msg| msg.type_() == gst::MessageType::Eos);
        let samples = self.samples.load(Ordering::Relaxed);
        let path = self.path.clone();
        drop(self);
        if !ok || samples == 0 {
            let _ = std::fs::remove_file(path);
            return None;
        }
        Some(Duration::from_millis(samples * 1000 / RATE))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        shut(&self.pipeline);
    }
}

struct Player {
    playbin: gst::Element,
    path: PathBuf,
    length: u32,
    rate: f64,
    at: u32,
    finished: bool,
}

impl Player {
    /// Loads `path` and waits until it is ready to play; `sink` replaces the system's output.
    fn load(path: &Path, sink: Option<gst::Element>) -> anyhow::Result<Self> {
        let uri = glib::filename_to_uri(path, None)?;
        // `soft-volume` keeps `gain` in the app instead of moving the sound server's slider.
        let playbin = gst::ElementFactory::make("playbin").property("uri", uri).property_from_str("flags", "audio+soft-volume").build()?;
        if let Some(gain) = GAIN.with_borrow(|g| g.as_ref().filter(|(p, _)| p == path).map(|&(_, gain)| gain)) {
            playbin.set_property("volume", gain);
        }
        // Faster speech keeps its pitch. Mono goes out as stereo at full level on both sides:
        // a mono stream can reach PipeWire's stereo outputs attenuated by its channel mixer.
        let tempo = gst::parse::bin_from_description("audioconvert ! scaletempo ! audioconvert ! capsfilter caps=audio/x-raw,channels=2", true)?;
        playbin.set_property("audio-filter", tempo);
        if let Some(sink) = sink {
            playbin.set_property("audio-sink", sink);
        }
        let mut player = Player { playbin, path: path.to_owned(), length: 0, rate: 1., at: 0, finished: false };
        player.playbin.set_state(gst::State::Paused)?;
        let loaded = player.playbin.state(WAIT).0;
        if let Some(e) = player.playbin.bus().and_then(|bus| error(&bus)) {
            anyhow::bail!(e);
        }
        loaded?;
        // Some files know their length only a moment after they are ready.
        for _ in 0..10 {
            if let Some(length) = player.playbin.query_duration::<gst::ClockTime>() {
                player.length = u32::try_from(length.mseconds()).unwrap_or(u32::MAX);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(player)
    }

    fn play(&mut self, from: f32, speed: f32) -> anyhow::Result<u32> {
        self.rate = speed.into();
        let at = ((from * self.length as f32) as u32).min(self.length.saturating_sub(1));
        self.goto(at)?;
        self.playbin.set_state(gst::State::Playing)?;
        Ok(self.length)
    }

    fn goto(&mut self, ms: u32) -> anyhow::Result<()> {
        (self.at, self.finished) = (ms, false);
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        self.playbin.seek(self.rate, flags, gst::SeekType::Set, gst::ClockTime::from_mseconds(ms.into()), gst::SeekType::None, gst::ClockTime::NONE)?;
        Ok(())
    }

    fn pause(&self) {
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    fn playing(&self) -> bool {
        self.playbin.current_state() == gst::State::Playing && !self.finished
    }

    fn position(&mut self) -> Option<u32> {
        if let Some(msg) = self.playbin.bus().and_then(|bus| bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])) {
            if let gst::MessageView::Error(e) = msg.view() {
                log::warn!("voice message: {} ({:?})", e.error(), e.debug());
            }
            self.finished = true;
        }
        if self.finished {
            return None;
        }
        // Unknown for a moment after a seek: the last known place stands.
        if let Some(at) = self.playbin.query_position::<gst::ClockTime>() {
            self.at = u32::try_from(at.mseconds()).unwrap_or(u32::MAX);
        }
        Some(self.at)
    }

    fn seek(&mut self, ms: u32) {
        if let Err(e) = self.goto(ms) {
            log::warn!("seek voice message: {e}");
        }
    }

    fn speed(&mut self, speed: f32) {
        if self.playing() {
            self.rate = speed.into();
            let at = self.position().unwrap_or(self.at);
            self.seek(at);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        shut(&self.playbin);
    }
}

#[cfg(test)]
mod tests {
    //! Headless: a test tone in, a fake sink out. Nothing reaches a sound server.
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use gstreamer as gst;
    use gstreamer::prelude::*;

    use super::{MAX_PEAKS, Peaks, Player, RATE, Recorder, WINDOW, peak};

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unisend-voice-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Records `ms` of a tone through the recorder's own pipeline.
    fn record(path: &Path, ms: u64) -> (Recorder, Duration) {
        assert!(super::init(), "GStreamer with its base and good plugins");
        let buffers = ms * RATE / 1000 / 480;
        let recorder = Recorder::start(&format!("audiotestsrc num-buffers={buffers} samplesperbuffer=480 volume=0.5"), path).unwrap();
        // The tone ends by itself, faster than real time; `finish` then finds its end on the bus.
        let started = Instant::now();
        while recorder.samples.load(std::sync::atomic::Ordering::Relaxed) < buffers * 480 {
            assert!(started.elapsed() < Duration::from_secs(5), "the tone never reached the encoder");
            std::thread::sleep(Duration::from_millis(10));
        }
        (recorder, Duration::from_millis(ms))
    }

    /// Decodes an Ogg Opus file and returns its channels, rate and length in samples.
    fn decode(path: &Path) -> (i32, i32, u64) {
        let pipeline = gst::parse::launch(&format!(
            "filesrc location={} ! oggdemux ! opusdec ! audio/x-raw,format=S16LE ! fakesink name=out",
            path.display()
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let samples = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let caps = std::sync::Arc::new(std::sync::Mutex::new((0, 0)));
        let pad = pipeline.by_name("out").unwrap().static_pad("sink").unwrap();
        let (s, c) = (samples.clone(), caps.clone());
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            if let (Some(buffer), Some(caps)) = (info.buffer(), pad.current_caps()) {
                let st = caps.structure(0).unwrap();
                let channels = st.get::<i32>("channels").unwrap();
                *c.lock().unwrap() = (channels, st.get::<i32>("rate").unwrap());
                s.fetch_add(buffer.size() as u64 / 2 / channels as u64, std::sync::atomic::Ordering::Relaxed);
            }
            gst::PadProbeReturn::Ok
        });
        pipeline.set_state(gst::State::Playing).unwrap();
        let msg = pipeline.bus().unwrap().timed_pop_filtered(super::WAIT, &[gst::MessageType::Eos, gst::MessageType::Error]).unwrap();
        assert_eq!(msg.type_(), gst::MessageType::Eos, "{msg:?}");
        pipeline.set_state(gst::State::Null).unwrap();
        let (channels, rate) = *caps.lock().unwrap();
        (channels, rate, samples.load(std::sync::atomic::Ordering::Relaxed))
    }

    #[test]
    fn peak_is_the_loudest_sample() {
        assert_eq!(peak(&[]), 0);
        assert_eq!(peak(&[0x10, 0x00, 0xf0, 0xff]), 16);
        assert_eq!(peak(&i16::MIN.to_le_bytes()), 32767);
        assert_eq!(peak(&i16::MAX.to_le_bytes()), 32767);
        // A trailing odd byte is not a sample.
        assert_eq!(peak(&[0x00, 0x01, 0x7f]), 256);
    }

    #[test]
    fn peaks_stay_bounded() {
        let mut peaks = Peaks::default();
        peaks.push(&[&16384i16.to_le_bytes()[..], &[0; 2 * (WINDOW - 1)]].concat());
        peaks.push(&[0; 2 * WINDOW + 2]);
        assert_eq!(peaks.levels.len(), 2);
        let levels = peaks.finish();
        assert_eq!(levels.len(), 3);
        assert!((levels[0] - 0.5).abs() < 0.01 && levels[1] == 0.);
        let mut long = Peaks::default();
        (0..3 * MAX_PEAKS).for_each(|_| long.push(&[0xff; 2 * WINDOW]));
        assert!(long.levels.len() < MAX_PEAKS);
    }

    #[test]
    fn wave_decodes_a_recording() {
        let path = dir("wave").join("voice.ogg");
        let (recorder, _) = record(&path, 2000);
        recorder.finish().unwrap();
        let levels = super::wave(&path).unwrap();
        // 2 s in 50 ms stretches; a half-volume tone peaks near 0.5.
        assert!(levels.len().abs_diff(40) <= 2, "{} peaks", levels.len());
        let peak = levels.iter().copied().fold(0., f32::max);
        assert!((0.4..0.65).contains(&peak), "peak {peak}");
        assert!(super::wave(&path.with_extension("missing")).is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn recording_is_mono_ogg_opus_of_its_length() {
        let path = dir("rec").join("voice.ogg");
        let (recorder, length) = record(&path, 2000);
        assert!(recorder.peak.load(std::sync::atomic::Ordering::Relaxed) > 8000, "a half-volume tone peaks near 16384");
        assert_eq!(recorder.finish(), Some(length));
        assert_eq!(length, Duration::from_millis(2000));
        let head = std::fs::read(&path).unwrap();
        assert_eq!(&head[..4], b"OggS");
        assert_eq!(&head[28..36], b"OpusHead");
        assert_eq!(head[37], 1, "mono");
        let (channels, rate, samples) = decode(&path);
        assert_eq!((channels, rate), (1, 48_000));
        // The decoder drops the encoder's pre-skip and the padding of the last packet.
        assert!(samples.abs_diff(2 * RATE) <= 960, "{samples} samples decoded");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn player_seeks_pauses_and_ends() {
        let path = dir("play").join("voice.ogg");
        let (recorder, _) = record(&path, 3000);
        recorder.finish().unwrap();
        let sink = gst::ElementFactory::make("fakesink").property("sync", true).build().unwrap();
        let mut player = Player::load(&path, Some(sink)).unwrap();
        assert!(player.length.abs_diff(3000) <= 20, "{} ms", player.length);
        // From the middle at double speed: the second half takes about 0.75 s.
        let started = Instant::now();
        assert_eq!(player.play(0.5, 2.).unwrap(), player.length);
        std::thread::sleep(Duration::from_millis(300));
        let at = player.position().unwrap();
        assert!((1600..=2400).contains(&at), "at {at} ms");
        player.pause();
        std::thread::sleep(Duration::from_millis(200));
        let paused = player.position().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(player.position(), Some(paused), "paused");
        player.seek(2500);
        player.playbin.set_state(gst::State::Playing).unwrap();
        while player.position().is_some() {
            assert!(started.elapsed() < Duration::from_secs(5), "never ended");
            std::thread::sleep(Duration::from_millis(50));
        }
        // Played again after the end, from the start.
        assert_eq!(player.play(0., 1.).unwrap(), player.length);
        std::thread::sleep(Duration::from_millis(200));
        assert!(player.position().is_some_and(|at| at < 1000));
        drop(player);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
