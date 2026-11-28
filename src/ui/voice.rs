//! Voice messages. On Android, holding the microphone (in the send button's place while the composer
//! is empty) records; letting go sends, sliding left throws it away, sliding up keeps it recording
//! without the finger. Voice messages and audio files play inside their bubble, one at a time.
//! `dev.gpui.beeper.Voice` owns the recorder and the player. On Linux, `linux.rs` does the same
//! through GStreamer, and a click on the microphone records at once, as if slid up to keep going.
//! Windows and macOS have no microphone button and open audio in the system's player.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, sync_channel};

use super::*;
use crate::backend::voice::{BARS, resample};
use crate::i18n::t;

/// Whether audio plays and records inside the app.
#[cfg(target_os = "android")]
fn inline() -> bool {
    true
}
#[cfg(not(target_os = "android"))]
fn inline() -> bool {
    native::ready()
}
/// How often the level is read while recording, and the position while playing.
const TICK: Duration = Duration::from_millis(100);
/// Longest recording; the recorder stops itself here as well (`MAX_MS` in Voice.java).
const MAX_RECORDING: Duration = Duration::from_secs(15 * 60);
/// Levels kept for one recording: one per tick, so at most 36 KB.
const MAX_LEVELS: usize = (MAX_RECORDING.as_millis() / TICK.as_millis()) as usize;
/// A hold shorter than this was a tap: it teaches instead of sending.
const MIN_HOLD: Duration = Duration::from_millis(400);
/// How far the finger slides left to throw the recording away, and up to keep it going.
const CANCEL_X: f32 = 120.;
const LOCK_Y: f32 = 80.;
/// Under this far the held finger moves nothing; past it the hold follows one way only.
const HOLD_SLOP: f32 = 10.;

/// What a held mic button does with a finger moved `(x, y)` from where it came down: left toward
/// cancel or up toward lock, whichever way it went more, never both, as in iOS chat apps.
fn held_drag(x: f32, y: f32) -> (f32, f32) {
    if x.hypot(y) < HOLD_SLOP {
        (0., 0.)
    } else if x.abs() > y.abs() {
        (x.min(0.), 0.)
    } else {
        (0., y.min(0.))
    }
}
/// Downloaded files kept for playing again, newest last.
const MAX_FILES: usize = 8;
/// Waveforms and gains worked out from downloaded files, newest last: under 4 KB.
const MAX_WAVES: usize = 64;
/// The loudest a quiet file is brought up to, and by at most how much (12 dB).
const TARGET_PEAK: f32 = 0.9;
const MAX_GAIN: f32 = 4.;
pub(super) const SPEEDS: [(f32, &str); 3] = [(1., "1×"), (1.5, "1.5×"), (2., "2×")];
/// The waveform's bars: 2 px wide with 2 px between them.
const BAR_W: f32 = 2.;
const WAVE_W: f32 = BARS as f32 * 4. - 2.;
const WAVE_H: f32 = 28.;
/// Levels shown while recording, the newest on the right.
const LIVE_BARS: usize = 28;
/// The player's id for a recording being listened to before it is sent.
const DRAFT: &str = "voice-draft";

#[derive(Default)]
pub(super) struct Voice {
    rec: Option<Rec>,
    play: Option<Play>,
    /// Index into `SPEEDS`; kept for every message until the app closes.
    speed: usize,
    files: VecDeque<(String, PathBuf)>,
    /// Per message: the bars drawn from its file, empty when the event carries its own, and the gain it plays at.
    waves: VecDeque<(String, Vec<u8>, f32)>,
    /// A message being downloaded to play, from this share of its length.
    want: Option<(String, f32)>,
    /// The microphone was refused: the composer says so until dismissed or allowed.
    denied: bool,
    asking: bool,
    /// A tap on the microphone shows how to use it, until then.
    hint: Option<Instant>,
    composer_empty: (bool, bool),
    /// Tells a stale answer from the recorder or player from the current one.
    epoch: u64,
    /// What the held recorder drew last, drawn again while it leaves; not its levels.
    shown: Option<Shown>,
}

#[derive(Clone, Copy)]
struct Shown {
    drag: (f32, f32),
    ring: f32,
    elapsed: Duration,
    /// Slid away rather than sent: the row keeps going left as it fades.
    thrown: bool,
}

struct Rec {
    epoch: u64,
    started: Instant,
    state: RecState,
    path: Option<PathBuf>,
    levels: Vec<f32>,
    /// Where the finger is from where it went down; only left and up count.
    drag: (f32, f32),
    /// The finger lifted before the recorder had started.
    lifted: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum RecState {
    Holding,
    Locked,
    /// Stopping, then waiting to be sent or thrown away.
    Stopped(Option<Duration>),
    /// Stopping, to be sent as soon as it has stopped; nothing is drawn for it.
    Sending,
}

struct Play {
    epoch: u64,
    id: String,
    path: PathBuf,
    /// Milliseconds; 0 until the player has loaded the file.
    length: u32,
    at: u32,
    playing: bool,
    /// A finger on the waveform, as a share of its width.
    scrub: Option<f32>,
}

impl Play {
    fn progress(&self) -> f32 {
        self.scrub.unwrap_or(if self.length == 0 { 0. } else { self.at as f32 / self.length as f32 })
    }
}

/// A recorder amplitude (0 to 32767) as a bar height, 0 to 1, on a 50 dB scale as ears hear it.
fn loudness(amplitude: i32) -> f32 {
    if amplitude <= 0 {
        return 0.;
    }
    ((20. * (amplitude as f32 / 32767.).log10() + 50.) / 50.).clamp(0., 1.)
}

impl BeeperApp {
    /// Whether the send button is a microphone: Android or Linux with GStreamer, an open chat that
    /// takes voice notes, nothing typed or waiting in the tray, not editing.
    pub(super) fn mic_shown(&self, cx: &App) -> bool {
        inline()
            && self.open.is_some()
            && self.caps().voice
            && !matches!(self.compose, Compose::Edit(_))
            && self.composer.read(cx).value().is_empty()
            && self.tray_room().is_none_or(|r| self.open.as_ref() != Some(r))
    }

    /// The composer turned empty or stopped being empty, or blank: the send button changes.
    pub(super) fn voice_typed(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value();
        let empty = (text.is_empty(), text.trim().is_empty());
        if empty != std::mem::replace(&mut self.voice.composer_empty, empty) {
            cx.notify();
        }
    }

    /// The press on the microphone, laid over the send button. A touch that starts there is claimed
    /// before the list can take it as a scroll, and followed until it lifts.
    pub(super) fn mic_touch(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                window.on_mouse_event(move |e: &TouchDragEvent, phase, window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    if e.phase == TouchPhase::Started {
                        if window.default_prevented() || !hitbox.is_hovered(window) {
                            return;
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                    let _ = weak.update(cx, |this, cx| {
                        // Another element's drag (a waveform's) passes here as well.
                        let holding = this.voice.rec.as_ref().is_some_and(|r| r.state == RecState::Holding);
                        match e.phase {
                            TouchPhase::Started => this.mic_down(cx),
                            TouchPhase::Moved => this.mic_moved(e.position - e.start_position, cx),
                            TouchPhase::Ended => this.mic_up(cx),
                            TouchPhase::Cancelled if holding => this.cancel_recording(cx),
                            TouchPhase::Cancelled => {}
                        }
                    });
                });
            },
        )
        .absolute()
        .inset_0()
    }

    fn mic_down(&mut self, cx: &mut Context<Self>) {
        if self.open.is_none() || self.voice.rec.is_some() {
            return;
        }
        self.voice.hint = None;
        if !native::access() {
            return self.ask_mic(cx);
        }
        #[cfg(target_os = "android")]
        motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Medium);
        self.stop_player();
        self.voice.epoch += 1;
        let epoch = self.voice.epoch;
        self.voice.rec = Some(Rec { epoch, started: Instant::now(), state: RecState::Holding, path: None, levels: Vec::new(), drag: (0., 0.), lifted: false });
        self.voice.shown = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = run(native::start).await.flatten();
            let started = this.update(cx, |this, cx| {
                let Some(rec) = this.voice.rec.as_mut().filter(|r| r.epoch == epoch) else {
                    // Thrown away while starting. A newer recording has already replaced it.
                    if path.is_some() && this.voice.rec.is_none() {
                        post(native::cancel);
                    }
                    return false;
                };
                let Some(path) = path else {
                    this.voice.rec = None;
                    this.error = Some(t!("Could not start recording").into());
                    cx.notify();
                    return false;
                };
                rec.path = Some(path);
                rec.started = Instant::now();
                if rec.lifted {
                    this.mic_up(cx);
                }
                this.voice.rec.as_ref().is_some_and(|r| r.epoch == epoch)
            });
            if !matches!(started, Ok(true)) {
                return;
            }
            loop {
                cx.background_executor().timer(TICK).await;
                let Some(amplitude) = run(native::level).await else { return };
                let going = this.update(cx, |this, cx| {
                    let Some(rec) = this.voice.rec.as_mut().filter(|r| r.epoch == epoch && !matches!(r.state, RecState::Stopped(_) | RecState::Sending)) else {
                        return false;
                    };
                    if rec.levels.len() < MAX_LEVELS {
                        rec.levels.push(loudness(amplitude));
                    }
                    if rec.started.elapsed() >= MAX_RECORDING {
                        let send = rec.state == RecState::Holding;
                        this.stop_recording(send, cx);
                    }
                    cx.notify();
                    true
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    fn mic_moved(&mut self, by: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(rec) = self.voice.rec.as_mut().filter(|r| r.state == RecState::Holding) else { return };
        rec.drag = held_drag(f32::from(by.x), f32::from(by.y));
        if -rec.drag.0 >= CANCEL_X {
            self.cancel_recording(cx);
            #[cfg(target_os = "android")]
            motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Medium);
        } else if -rec.drag.1 >= LOCK_Y {
            (rec.state, rec.drag) = (RecState::Locked, (0., 0.));
            #[cfg(target_os = "android")]
            motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Medium);
        }
        cx.notify();
    }

    fn mic_up(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.voice.rec.as_mut().filter(|r| r.state == RecState::Holding) else { return };
        if rec.path.is_none() {
            rec.lifted = true;
        } else if rec.started.elapsed() < MIN_HOLD {
            self.cancel_recording(cx);
            // The hint takes the bar's place: nothing leaves.
            self.voice.shown = None;
            self.show_hint(cx);
        } else {
            self.stop_recording(true, cx);
        }
    }

    fn show_hint(&mut self, cx: &mut Context<Self>) {
        let at = Instant::now();
        self.voice.hint = Some(at);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                if this.voice.hint == Some(at) {
                    this.voice.hint = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Ends the recording; `send` sends it, otherwise it waits to be listened to.
    fn stop_recording(&mut self, send: bool, cx: &mut Context<Self>) {
        let Some(rec) = self.voice.rec.as_mut().filter(|r| r.path.is_some() && !matches!(r.state, RecState::Stopped(_) | RecState::Sending)) else { return };
        // Sending, the review panel is not shown for the moment the recorder takes to stop.
        rec.state = if send { RecState::Sending } else { RecState::Stopped(None) };
        let (epoch, path) = (rec.epoch, rec.path.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let length = run(native::stop).await.flatten();
            let _ = this.update(cx, |this, cx| {
                let Some(rec) = this.voice.rec.as_mut().filter(|r| r.epoch == epoch) else {
                    // Thrown away while it was stopping.
                    if let Some(path) = path.filter(|_| length.is_some()) {
                        post(move || native::delete(&path));
                    }
                    return;
                };
                match length {
                    None => {
                        this.voice.rec = None;
                        this.error = Some(t!("Nothing was recorded").into());
                    }
                    Some(length) if send => {
                        rec.state = RecState::Stopped(Some(length));
                        this.send_recording(cx);
                    }
                    Some(length) => rec.state = RecState::Stopped(Some(length)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send_recording(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let Some(Rec { state: RecState::Stopped(Some(duration)), path: Some(path), levels, .. }) = self.voice.rec.take() else { return };
        if self.voice.play.as_ref().is_some_and(|p| p.id == DRAFT) {
            self.stop_player();
        }
        if self.attach_queue.len() >= gallery::MAX_QUEUED {
            self.error = Some(t!("Too many attachments waiting, try again when these are sent").into());
            post(move || native::delete(&path));
            return cx.notify();
        }
        let reply_to = match std::mem::replace(&mut self.compose, Compose::New) {
            Compose::Reply(id) => Some(id),
            other => {
                self.compose = other;
                None
            }
        };
        self.uploads += 1;
        self.attach_queue.push_back(Cmd::Voice { room, path, duration, levels, reply_to });
        self.next_attach(cx);
    }

    /// Throws the recording away, whatever state it is in.
    pub(super) fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.voice.rec.take() else { return };
        if self.voice.play.as_ref().is_some_and(|p| p.id == DRAFT) {
            self.stop_player();
        }
        if let Some(shown) = self.voice.shown.as_mut().filter(|_| rec.state == RecState::Holding) {
            (shown.drag, shown.thrown) = (rec.drag, true);
        }
        match (rec.state, rec.path) {
            (RecState::Stopped(Some(_)), Some(path)) => post(move || native::delete(&path)),
            (_, Some(_)) => post(native::cancel),
            // Still starting: the start's answer finds no recording and cancels it.
            (_, None) => {}
        }
        cx.notify();
    }

    fn ask_mic(&mut self, cx: &mut Context<Self>) {
        native::ask();
        if std::mem::replace(&mut self.voice.asking, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let mut left = false;
            // Two minutes at most; the denied line offers to ask again.
            for _ in 0..240 {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let granted = native::access();
                let front = crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed);
                let back = left && front;
                left |= !front;
                if !granted && !back {
                    continue;
                }
                let _ = this.update(cx, |this, cx| {
                    this.voice.denied = !granted;
                    cx.notify();
                });
                break;
            }
            let _ = this.update(cx, |this, _| this.voice.asking = false);
        })
        .detach();
    }

    /// Linux: loads GStreamer off the UI thread, and on quit stops what records or plays (an
    /// unsent recording is deleted) before the process ends.
    #[cfg(target_os = "linux")]
    pub(super) fn voice_setup(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            if run(native::init).await == Some(true) {
                let _ = this.update(cx, |_, cx| cx.notify());
            }
        })
        .detach();
        cx.on_app_quit(|this, cx| {
            this.stop_voice(cx);
            let done = run(|| ());
            async move {
                done.await;
            }
        })
        .detach();
    }

    /// A click on the microphone: a mouse cannot hold and slide, so it records until stopped or sent.
    pub(super) fn mic_click(&mut self, cx: &mut Context<Self>) {
        if self.voice.rec.is_some() {
            return;
        }
        self.mic_down(cx);
        if let Some(rec) = self.voice.rec.as_mut().filter(|r| r.state == RecState::Holding) {
            rec.state = RecState::Locked;
        }
    }

    /// The chat closed: nothing records or plays on.
    pub(super) fn stop_voice(&mut self, cx: &mut Context<Self>) {
        self.cancel_recording(cx);
        self.stop_player();
        (self.voice.want, self.voice.denied, self.voice.hint) = (None, false, None);
        self.voice.epoch += 1;
    }

    pub(super) fn stop_player(&mut self) {
        if self.voice.play.take().is_some() {
            post(native::stop_player);
        }
    }

    /// Plays or pauses a message's audio, downloading it first; the draft has no `media`.
    fn toggle_play(&mut self, id: &str, media: Option<&Media>, cx: &mut Context<Self>) {
        match self.voice.play.as_mut().filter(|p| p.id == id) {
            Some(play) if play.playing => {
                play.playing = false;
                post(native::pause);
                cx.notify();
            }
            Some(play) => {
                let (path, from) = (play.path.clone(), play.progress());
                self.start_player(id, path, from, cx);
            }
            None => self.load(id, media, 0., cx),
        }
    }

    fn load(&mut self, id: &str, media: Option<&Media>, from: f32, cx: &mut Context<Self>) {
        let Some(media) = media else {
            let path = self.voice.rec.as_ref().and_then(|r| r.path.clone());
            return path.into_iter().for_each(|path| self.start_player(DRAFT, path, from, cx));
        };
        if let Some((_, path)) = self.voice.files.iter().find(|(f, _)| f == id) {
            return self.start_player(id, path.clone(), from, cx);
        }
        self.voice.want = Some((id.to_owned(), from));
        if !self.fetching.contains(id) && self.send(Cmd::Fetch(id.to_owned(), media.clone()), cx) {
            self.fetching.insert(id.to_owned());
        }
    }

    /// A file arrived from the media cache; true when it is one this player handles.
    pub(super) fn voice_fetched(&mut self, id: &str, path: &Path, cx: &mut Context<Self>) -> bool {
        let audio = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.as_ref()).is_some_and(|m| matches!(m.kind, MediaKind::Voice | MediaKind::Audio));
        if !inline() || !audio {
            return false;
        }
        if self.voice.files.len() >= MAX_FILES {
            self.voice.files.pop_front();
        }
        self.voice.files.push_back((id.to_owned(), path.to_owned()));
        if !self.voice.waves.iter().any(|(w, ..)| w == id) {
            self.measure(id, path, cx);
        }
        if let Some((_, from)) = self.voice.want.take_if(|(want, _)| want == id) {
            self.start_player(id, path.to_owned(), from, cx);
        }
        true
    }

    /// Decodes a downloaded file for the bars its event lacks and for the gain that brings it up to `TARGET_PEAK`.
    fn measure(&mut self, id: &str, path: &Path, cx: &mut Context<Self>) {
        let (id, path) = (id.to_owned(), path.to_owned());
        cx.spawn(async move |this, cx| {
            let file = path.clone();
            let Some(levels) = cx.background_spawn(async move { native::wave(&file) }).await else { return };
            let peak = levels.iter().copied().fold(0., f32::max);
            if peak <= 0. {
                return;
            }
            let gain = (TARGET_PEAK / peak).clamp(1., MAX_GAIN);
            post(move || native::gain(&path, gain));
            let _ = this.update(cx, |this, cx| {
                let carried = this.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.as_ref()).is_some_and(|m| !m.waveform.is_empty());
                let bars = if carried { Vec::new() } else { resample(&levels, BARS).iter().map(|v| (v / peak * 255.).round() as u8).collect() };
                if this.voice.waves.len() >= MAX_WAVES {
                    this.voice.waves.pop_front();
                }
                this.voice.waves.push_back((id, bars, gain));
                cx.notify();
            });
        })
        .detach();
    }

    fn start_player(&mut self, id: &str, path: PathBuf, from: f32, cx: &mut Context<Self>) {
        self.voice.epoch += 1;
        let epoch = self.voice.epoch;
        let length = self.voice.play.as_ref().filter(|p| p.id == id).map_or(0, |p| p.length);
        let at = (from * length as f32) as u32;
        self.voice.play = Some(Play { epoch, id: id.to_owned(), path: path.clone(), length, at, playing: true, scrub: None });
        let speed = SPEEDS[self.voice.speed].0;
        if let Some(&(.., gain)) = self.voice.waves.iter().find(|(w, ..)| w == id) {
            let path = path.clone();
            post(move || native::gain(&path, gain));
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let file = path.clone();
            let length = run(move || native::play(&path, from, speed)).await.flatten();
            let playing = this.update(cx, |this, cx| {
                let Some(play) = this.voice.play.as_mut().filter(|p| p.epoch == epoch) else { return false };
                match length {
                    Some(length) => (play.length, play.at) = (length, (from * length as f32) as u32),
                    None => {
                        let id = this.voice.play.take().map(|p| p.id);
                        this.voice.files.retain(|(f, _)| Some(f) != id.as_ref());
                        let media = this.msgs.iter().find(|m| Some(&m.id) == id.as_ref()).and_then(|m| m.media.as_ref());
                        match media {
                            // GStreamer may lack this file's decoder; the system's player may have one.
                            Some(media) if cfg!(not(target_os = "android")) => {
                                let mime = media.mime.clone();
                                crate::open_file(cx, &file, mime.as_deref());
                            }
                            _ => this.error = Some(t!("Could not play this audio").into()),
                        }
                    }
                }
                cx.notify();
                length.is_some()
            });
            if !matches!(playing, Ok(true)) {
                return;
            }
            loop {
                cx.background_executor().timer(TICK).await;
                let Some(at) = run(native::position).await else { return };
                let going = this.update(cx, |this, cx| {
                    let Some(play) = this.voice.play.as_mut().filter(|p| p.epoch == epoch && p.playing) else { return false };
                    match at {
                        Some(at) => play.at = at,
                        // The end: back to the start, ready to play again.
                        None => (play.playing, play.at) = (false, 0),
                    }
                    cx.notify();
                    at.is_some()
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// Moves playback to a share of the length; a message not loaded starts playing there.
    fn seek(&mut self, id: &str, media: &Media, to: f32, cx: &mut Context<Self>) {
        let to = to.clamp(0., 1.);
        match self.voice.play.as_mut().filter(|p| p.id == id && p.length > 0) {
            Some(play) => {
                (play.at, play.scrub) = ((to * play.length as f32) as u32, None);
                let at = play.at;
                post(move || native::seek(at));
                cx.notify();
            }
            None => self.load(id, Some(media), to, cx),
        }
    }

    fn scrub(&mut self, id: &str, to: Option<f32>, cx: &mut Context<Self>) {
        if let Some(play) = self.voice.play.as_mut().filter(|p| p.id == id) {
            play.scrub = to.map(|to| to.clamp(0., 1.));
            cx.notify();
        }
    }

    fn next_speed(&mut self, cx: &mut Context<Self>) {
        self.voice.speed = (self.voice.speed + 1) % SPEEDS.len();
        let speed = SPEEDS[self.voice.speed].0;
        post(move || native::speed(speed));
        cx.notify();
    }

    /// A voice message or audio file in its bubble: play, the waveform to seek on, time and speed.
    pub(super) fn render_voice(&self, m: &Msg, media: &Media, cx: &mut Context<Self>) -> AnyElement {
        let p = inside_of(m, Palette::of(cx));
        let play = self.voice.play.as_ref().filter(|play| play.id == m.id);
        let total = play.filter(|play| play.length > 0).map(|play| Duration::from_millis(play.length.into())).or(media.duration);
        let time = match play {
            Some(play) => length(Duration::from_millis(play.at.into())),
            None => total.map(length).unwrap_or_default(),
        };
        let measured = self.voice.waves.iter().find(|(w, ..)| *w == m.id).map(|(_, bars, _)| bars).filter(|bars| !bars.is_empty());
        let bars = if media.waveform.is_empty() { measured } else { Some(&media.waveform) };
        let heights: Vec<f32> = bars.map_or(vec![0.; BARS], |bars| bars.iter().map(|&h| f32::from(h) / 255.).collect());
        let (id, owned) = (m.id.clone(), media.clone());
        let button = cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            if inline() {
                this.toggle_play(&id, Some(&owned), cx);
            } else {
                this.open_media(id.clone(), owned.clone(), cx);
            }
        });
        // Without the player in the app, the file goes to the system's player, from anywhere on the row.
        let (on_button, on_row) = if inline() { (Some(button), None) } else { (None, Some(button)) };
        let detail = if self.fetching.contains(&m.id) { Some(t!("Downloading…").to_owned()) } else { None };
        h_flex()
            .id(SharedString::from(format!("{}-voice", m.id)))
            .gap(px(10.))
            .py(px(2.))
            .when_some(on_row, |el, f| el.cursor_pointer().on_click(f))
            .child(play_button(play.is_some_and(|p| p.playing), detail.is_some(), p).when_some(on_button, |el, f| el.on_click(f)))
            .child(
                v_flex()
                    .min_w_0()
                    .gap(px(2.))
                    .when(media.kind == MediaKind::Audio, |el| el.child(div().truncate().text_size(fs(14.)).child(media.name.clone())))
                    .child(self.seekable(m, media, wave(heights, play.map_or(0., Play::progress), p.text), cx))
                    .child(
                        h_flex()
                            .h(px(16.))
                            .justify_between()
                            .text_size(fs(12.))
                            .font_features(tabular())
                            .child(div().opacity(0.7).child(detail.unwrap_or(time)))
                            .when(inline() && play.is_some(), |el| el.child(self.speed_chip(&m.id, p, cx))),
                    ),
            )
            .into_any_element()
    }

    fn speed_chip(&self, id: &str, p: Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // A 44 tall target around a 20 tall chip, without making the line taller.
        div()
            .id(SharedString::from(format!("{id}-speed")))
            .h(px(TAP))
            .my(px((16. - TAP) / 2.))
            .px(px(4.))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.next_speed(cx);
            }))
            .child(div().px(px(6.)).h(px(20.)).flex().items_center().rounded_full().bg(p.text.opacity(0.15)).child(SPEEDS[self.voice.speed].1))
    }

    /// The waveform, with a tap to seek and, while its audio is loaded, a drag to scrub.
    fn seekable(&self, m: &Msg, media: &Media, wave: Div, cx: &mut Context<Self>) -> Div {
        if !inline() {
            return wave;
        }
        let weak = cx.entity().downgrade();
        let loaded = self.voice.play.as_ref().is_some_and(|p| p.id == m.id && p.length > 0);
        let (id, media) = (m.id.clone(), media.clone());
        wave.relative().child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hitbox, window, _| {
                    let share = move |x: Pixels| f32::from(x - bounds.left()) / f32::from(bounds.size.width);
                    window.on_mouse_event({
                        let (weak, id, media, hitbox) = (weak.clone(), id.clone(), media.clone(), hitbox.clone());
                        move |e: &MouseDownEvent, phase, window, cx| {
                            if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                cx.stop_propagation();
                                let _ = weak.update(cx, |this, cx| this.seek(&id, &media, share(e.position.x), cx));
                            }
                        }
                    });
                    window.on_mouse_event(move |e: &TouchDragEvent, phase, window, cx| {
                        if !phase.bubble() {
                            return;
                        }
                        if e.phase == TouchPhase::Started {
                            if !loaded || window.default_prevented() || !hitbox.is_hovered(window) {
                                return;
                            }
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                        let _ = weak.update(cx, |this, cx| {
                            let ours = this.voice.play.as_ref().is_some_and(|p| p.id == id && p.scrub.is_some());
                            match e.phase {
                                TouchPhase::Started => this.scrub(&id, Some(share(e.position.x)), cx),
                                _ if !ours => {}
                                TouchPhase::Moved => this.scrub(&id, Some(share(e.position.x)), cx),
                                TouchPhase::Ended => this.seek(&id, &media, share(e.position.x), cx),
                                TouchPhase::Cancelled => this.scrub(&id, None, cx),
                            }
                        });
                    });
                },
            )
            .absolute()
            .inset_0(),
        )
    }

    /// Laid over the composer while recording, after a refusal, or after a tap on the microphone.
    pub(super) fn render_recorder(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let present = self.voice.rec.as_ref().is_some_and(|r| r.state != RecState::Sending);
        let presence = Presence::new("recorder", present).transition(if present { motion::enter() } else { motion::exit() }).sample(window, cx);
        let bar = || h_flex().absolute().inset_0().occlude().bg(p.ground).items_center().gap(px(6.)).pl(px(GUTTER - 10.)).pr(px(GUTTER));
        let Some(rec) = self.voice.rec.as_ref().filter(|_| present) else {
            match self.voice.shown.filter(|_| presence.should_render()) {
                Some(shown) => return Some(self.held(shown, None, presence.progress, window, cx)),
                None => self.voice.shown = None,
            }
            if self.voice.denied {
                return Some(
                    bar()
                        .pl(px(GUTTER))
                        .child(div().flex_1().min_w_0().text_size(fs(14.)).text_color(p.muted).child(t!("Voice messages need the microphone.")))
                        .child(Button::new("mic-allow").primary().h(px(TAP)).label(t!("Allow")).on_click(cx.listener(|this, _, _, cx| this.ask_mic(cx))))
                        .child(round_button("mic-dismiss", IconName::X, t!("Close"), cx).text_color(p.muted).on_click(cx.listener(|this, _, _, cx| {
                            this.voice.denied = false;
                            cx.notify();
                        })))
                        .into_any_element(),
                );
            }
            // Not occluding: the composer under it stays usable while it fades.
            return self.voice.hint.map(|_| {
                h_flex()
                    .absolute()
                    .inset_0()
                    .bg(p.ground)
                    .items_center()
                    .px(px(GUTTER))
                    .text_size(fs(14.))
                    .text_color(p.muted)
                    .child(t!("Hold to record, release to send."))
                    .with_animation("mic-hint", Animation::new(motion::ENTER).with_easing(motion::ease_out), |el, t| el.opacity(t))
                    .into_any_element()
            });
        };
        let elapsed = match rec.state {
            RecState::Stopped(Some(length)) => length,
            _ if rec.path.is_none() => Duration::ZERO,
            _ => rec.started.elapsed().min(MAX_RECORDING),
        };
        if rec.state == RecState::Holding {
            let (drag, level) = (rec.drag, rec.levels.last().copied().unwrap_or(0.));
            let ring = spring(("voice", "level"), level, motion::LEVEL, window, cx);
            let shown = Shown { drag, ring, elapsed, thrown: false };
            self.voice.shown = Some(shown);
            return Some(self.held(shown, Some(drag), presence.progress, window, cx));
        }
        // Only the held recorder plays its exit; the locked one and the review panel go at once.
        self.voice.shown = None;
        let rec = self.voice.rec.as_ref()?;
        let send = |id: &'static str| send_button(id, p);
        let clock = clock(elapsed, p);
        let el = match rec.state {
            RecState::Holding | RecState::Sending => return None,
            RecState::Locked | RecState::Stopped(_) => {
                let stopped = matches!(rec.state, RecState::Stopped(_));
                let draft = self.voice.play.as_ref().filter(|p| p.id == DRAFT);
                let heights: Vec<f32> = if stopped {
                    resample(&rec.levels, BARS)
                } else {
                    let tail = &rec.levels[rec.levels.len().saturating_sub(LIVE_BARS)..];
                    std::iter::repeat_n(0., LIVE_BARS - tail.len()).chain(tail.iter().copied()).collect()
                };
                bar()
                    .child(round_button("rec-delete", IconName::Trash, t!("Delete recording"), cx).text_color(p.negative).on_click(cx.listener(|this, _, _, cx| this.cancel_recording(cx))))
                    .when(stopped, |el| {
                        let playing = draft.is_some_and(|d| d.playing);
                        el.child(play_button(playing, rec.state == RecState::Stopped(None), p).on_click(cx.listener(|this, _, _, cx| this.toggle_play(DRAFT, None, cx))))
                    })
                    .when(!stopped, |el| el.child(clock))
                    .child(div().flex_1().min_w_0().flex().justify_center().overflow_hidden().child(wave(heights, if stopped { draft.map_or(0., Play::progress) } else { 1. }, p.text)))
                    .when(stopped, |el| el.child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.muted).child(length(elapsed))))
                    .when(!stopped, |el| {
                        el.child(
                            round_button("rec-stop", IconName::Square, t!("Stop recording"), cx)
                                .text_color(p.negative)
                                .on_click(cx.listener(|this, _, _, cx| this.stop_recording(false, cx))),
                        )
                    })
                    .child(send("rec-send").child(Icon::new(IconName::ArrowUp).size_5()).on_click(cx.listener(|this, _, _, cx| {
                        match this.voice.rec.as_ref().map(|r| r.state) {
                            Some(RecState::Stopped(Some(_))) => this.send_recording(cx),
                            Some(RecState::Locked) => this.stop_recording(true, cx),
                            _ => {}
                        }
                    })))
                    .with_animation("rec-locked", Animation::new(motion::ENTER).with_easing(motion::ease_out), |el, t| el.opacity(t))
                    .into_any_element()
            }
        };
        Some(el)
    }

    /// The recorder while the finger holds the microphone, and while it leaves after letting go
    /// (`held` None): the microphone follows the finger 1:1, then springs home. `v` is the bar's presence.
    fn held(&self, shown: Shown, held: Option<(f32, f32)>, v: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let live = held.is_some();
        let (dx, dy) = held.unwrap_or((0., 0.));
        let mic_x = spring(("voice", "mic-x"), dx, motion::RELEASE.with_travel(!live), window, cx);
        let mic_y = spring(("voice", "mic-y"), dy, motion::RELEASE.with_travel(!live), window, cx);
        // Arriving, the clock and the hint slide in from the right; slid away, the hint keeps going left.
        let enter = if live { 16. * (1. - v) } else { 0. };
        let gone = if shown.thrown { -40. * (1. - v) } else { 0. };
        let ring = TAP + 40. * shown.ring;
        let lock_shown = 1. + dy / LOCK_Y;
        h_flex()
            .absolute()
            .inset_0()
            // Leaving, it takes no input: the composer under it is already back.
            .when(live, |el| el.occlude())
            .bg(p.ground)
            .items_center()
            .gap(px(6.))
            .pl(px(GUTTER - 10.))
            .pr(px(GUTTER))
            .opacity(v)
            .child(clock(shown.elapsed, p).relative().left(px(enter)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_center()
                    .gap(px(4.))
                    .relative()
                    .left(px(shown.drag.0 + enter + gone))
                    .opacity((1. + shown.drag.0 / CANCEL_X).max(0.))
                    .text_size(fs(14.))
                    .text_color(p.faint)
                    .child(Icon::new(IconName::ChevronLeft).size_4())
                    .child(t!("Slide to cancel")),
            )
            .child(
                div()
                    .size(px(TAP))
                    .flex_none()
                    .relative()
                    .left(px(mic_x))
                    .top(px(mic_y))
                    .child(div().absolute().size(px(ring)).left(px((TAP - ring) / 2.)).top(px((TAP - ring) / 2.)).rounded_full().bg(p.text.opacity(0.18)))
                    .child(send_button("rec-mic", p).absolute().inset_0().child(Icon::new(IconName::Mic).size_5())),
            )
            // The lock waits above the button; it brightens as the finger nears.
            .when(live, |el| {
                el.child(
                    v_flex()
                        .absolute()
                        .right(px(GUTTER + 4.))
                        .bottom(px(TAP + 20.))
                        .w(px(TAP - 8.))
                        .py(px(10.))
                        .gap(px(4.))
                        .items_center()
                        .rounded_full()
                        .bg(p.surface)
                        .text_color(p.muted)
                        .child(Icon::new(IconName::Lock).size_4().opacity(0.6 + 0.4 * (1. - lock_shown)))
                        .child(Icon::new(IconName::ChevronUp).size_4()),
                )
            })
            .into_any_element()
    }
}

fn clock(elapsed: Duration, p: Palette) -> Div {
    let dot = div().size(px(8.)).flex_none().rounded_full().bg(p.negative).with_animation(
        "rec-dot",
        Animation::new(Duration::from_millis(1000)).repeat().with_max_fps(24.),
        |el, t| el.opacity(0.4 + 0.6 * (0.5 + 0.5 * (t * std::f32::consts::TAU).cos())),
    );
    h_flex().gap(px(8.)).pl(px(10.)).flex_none().text_size(fs(16.)).font_features(tabular()).child(dot).child(length(elapsed))
}

pub(super) fn send_button(id: &'static str, p: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(TAP))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(p.text)
        .text_color(p.ground)
        .cursor_pointer()
        .active(|el| el.bg(p.muted))
}

fn play_button(playing: bool, waiting: bool, p: Palette) -> Stateful<Div> {
    div()
        .id("voice-play")
        .size(px(40.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(p.text.opacity(0.12))
        .cursor_pointer()
        .when(waiting, |el| el.opacity(0.5))
        .child(Icon::new(if playing { IconName::Pause } else { IconName::Play }).size_5())
}

/// Bars for `heights` (0 to 1), the first `played` share of them in full ink.
fn wave(heights: Vec<f32>, played: f32, ink: Hsla) -> Div {
    let count = heights.len().max(1) as f32;
    h_flex().h(px(WAVE_H)).w(px(heights.len() as f32 * (BAR_W + 2.) - 2.)).max_w(px(WAVE_W)).gap(px(2.)).items_center().children(heights.into_iter().enumerate().map(
        move |(ix, h)| {
            let reached = (ix as f32 + 0.5) / count <= played;
            div().w(px(BAR_W)).h(px(3. + h * (WAVE_H - 3.))).flex_none().rounded_full().bg(if reached { ink } else { ink.opacity(0.35) })
        },
    ))
}

/// Queues a call into the recorder or player. They run on one thread in the order they were made,
/// so a pause cannot overtake the play it follows. At most 16 wait; a 17th waits for room.
fn post(job: impl FnOnce() + Send + 'static) {
    type Job = Box<dyn FnOnce() + Send>;
    static WORKER: OnceLock<Option<SyncSender<Job>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = sync_channel::<Job>(16);
        // GStreamer loads its plugins on the calling thread.
        let stack = if cfg!(target_os = "linux") { 1024 * 1024 } else { 256 * 1024 };
        let spawned = std::thread::Builder::new().name("voice".into()).stack_size(stack).spawn(move || rx.into_iter().for_each(|job| job()));
        spawned.map_err(|e| log::warn!("voice thread: {e}")).ok().map(|_| tx)
    });
    if let Some(worker) = worker {
        let _ = worker.send(Box::new(job));
    }
}

/// `post`, with the call's answer; `None` when the thread is gone.
fn run<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> impl Future<Output = Option<T>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    post(move || {
        let _ = tx.send(job());
    });
    async move { rx.await.ok() }
}

#[cfg(target_os = "android")]
mod native {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JClass, JObject, JValue};
    use jni::{jni_sig, jni_str};

    const CLASS: &str = "dev.gpui.beeper.Voice";

    /// Runs `f` with the `Voice` class and the activity; a failure is logged and is `None`.
    fn call<T>(what: &str, f: impl FnOnce(&mut jni::Env<'_>, &JClass<'_>, &JObject<'_>) -> jni::errors::Result<T>) -> Option<T> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            f(env, &class, &activity).map_err(|e| crate::java_error(env, e))
        })
        .map_err(|e| log::warn!("voice {what}: {e}"))
        .ok()
    }

    pub fn access() -> bool {
        call("access", |env, class, activity| {
            env.call_static_method(class, jni_str!("access"), jni_sig!("(Landroid/app/Activity;)Z"), &[JValue::Object(activity)])?.z()
        })
        .unwrap_or(false)
    }

    pub fn ask() {
        call("ask", |env, class, activity| env.call_static_method(class, jni_str!("ask"), jni_sig!("(Landroid/app/Activity;)V"), &[JValue::Object(activity)])?.v());
    }

    pub fn start() -> Option<PathBuf> {
        call("start", |env, class, activity| {
            let path = env.call_static_method(class, jni_str!("start"), jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"), &[JValue::Object(activity)])?.l()?;
            Ok((!path.is_null()).then(|| PathBuf::from(helpers::get_string(env, &path))))
        })
        .flatten()
    }

    pub fn level() -> i32 {
        call("level", |env, class, _| env.call_static_method(class, jni_str!("level"), jni_sig!("()I"), &[])?.i()).unwrap_or(0)
    }

    pub fn stop() -> Option<Duration> {
        let ms = call("stop", |env, class, _| env.call_static_method(class, jni_str!("stop"), jni_sig!("()J"), &[])?.j())?;
        u64::try_from(ms).ok().map(Duration::from_millis)
    }

    pub fn cancel() {
        call("cancel", |env, class, _| env.call_static_method(class, jni_str!("cancel"), jni_sig!("()V"), &[])?.v());
    }

    pub fn delete(path: &Path) {
        call("delete", |env, class, _| {
            let path = env.new_string(path.to_string_lossy())?;
            env.call_static_method(class, jni_str!("delete"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&path)])?.v()
        });
    }

    /// The length in ms, once playing.
    pub fn play(path: &Path, from: f32, speed: f32) -> Option<u32> {
        let ms = call("play", |env, class, activity| {
            let path = env.new_string(path.to_string_lossy())?;
            env.call_static_method(
                class,
                jni_str!("play"),
                jni_sig!("(Landroid/app/Activity;Ljava/lang/String;FF)I"),
                &[JValue::Object(activity), JValue::Object(&path), JValue::Float(from), JValue::Float(speed)],
            )?
            .i()
        })?;
        u32::try_from(ms).ok()
    }

    pub fn pause() {
        call("pause", |env, class, _| env.call_static_method(class, jni_str!("pause"), jni_sig!("()V"), &[])?.v());
    }

    /// Where playback is, in ms; `None` once it has ended.
    pub fn position() -> Option<u32> {
        let ms = call("position", |env, class, _| env.call_static_method(class, jni_str!("position"), jni_sig!("()I"), &[])?.i())?;
        u32::try_from(ms).ok()
    }

    pub fn seek(ms: u32) {
        let ms = i32::try_from(ms).unwrap_or(i32::MAX);
        call("seek", |env, class, _| env.call_static_method(class, jni_str!("seek"), jni_sig!("(I)V"), &[JValue::Int(ms)])?.v());
    }

    pub fn speed(speed: f32) {
        call("speed", |env, class, _| env.call_static_method(class, jni_str!("speed"), jni_sig!("(F)V"), &[JValue::Float(speed)])?.v());
    }

    /// The peaks of `path`, 0 to 1; `None` when it cannot be decoded.
    pub fn wave(path: &Path) -> Option<Vec<f32>> {
        call("wave", |env, class, _| {
            let path = env.new_string(path.to_string_lossy())?;
            let peaks = env.call_static_method(class, jni_str!("wave"), jni_sig!("(Ljava/lang/String;)[I"), &[JValue::Object(&path)])?.l()?;
            if peaks.is_null() {
                return Ok(None);
            }
            let peaks = env.cast_local::<jni::objects::JIntArray>(peaks)?;
            let mut buf = vec![0; peaks.len(env)?];
            peaks.get_region(env, 0, &mut buf)?;
            Ok(Some(buf.into_iter().map(|p| p.clamp(0, 32767) as f32 / 32767.).collect()))
        })
        .flatten()
    }

    /// `MediaPlayer` cannot play louder than recorded; the file plays as it is.
    pub fn gain(_: &Path, _: f32) {}

    pub fn stop_player() {
        call("stopPlayer", |env, class, _| env.call_static_method(class, jni_str!("stopPlayer"), jni_sig!("()V"), &[])?.v());
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as native;

#[cfg(not(any(target_os = "android", target_os = "linux")))]
mod native {
    //! Windows and macOS record and play nothing; these keep the shared code compiling.
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    pub fn ready() -> bool {
        false
    }
    pub fn access() -> bool {
        false
    }
    pub fn ask() {}
    pub fn start() -> Option<PathBuf> {
        None
    }
    pub fn level() -> i32 {
        0
    }
    pub fn stop() -> Option<Duration> {
        None
    }
    pub fn cancel() {}
    pub fn delete(_: &Path) {}
    pub fn play(_: &Path, _: f32, _: f32) -> Option<u32> {
        None
    }
    pub fn pause() {}
    pub fn position() -> Option<u32> {
        None
    }
    pub fn seek(_: u32) {}
    pub fn speed(_: f32) {}
    pub fn wave(_: &Path) -> Option<Vec<f32>> {
        None
    }
    pub fn gain(_: &Path, _: f32) {}
    pub fn stop_player() {}
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::loudness;

    #[test]
    fn a_held_mic_follows_one_way_at_a_time() {
        use super::held_drag;
        assert_eq!(held_drag(-6., -4.), (0., 0.), "inside the slop");
        assert_eq!(held_drag(-90., -40.), (-90., 0.), "mostly left: toward cancel only");
        assert_eq!(held_drag(-30., -70.), (0., -70.), "mostly up: toward lock only");
        assert_eq!(held_drag(50., 5.), (0., 0.), "right does nothing");
    }

    #[test]
    fn loudness_is_on_a_50_db_scale() {
        assert_eq!(loudness(0), 0.);
        assert_eq!(loudness(-5), 0.);
        assert!((loudness(32767) - 1.).abs() < 1e-6);
        // -20 dB is 60% up the scale; anything under -50 dB is silence.
        assert!((loudness(3277) - 0.6).abs() < 0.01);
        assert_eq!(loudness(10), 0.);
    }
}
