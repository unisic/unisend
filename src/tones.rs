//! Notification sounds: the tones the app ships (made by `scripts/sounds.py` into Android's
//! `res/raw`, which desktop includes from there), the user's own sounds, which one each kind of
//! chat plays, and handing that choice to the platform. Android gives each kind a channel with its
//! sound (`Notify.sounds`); Linux names the sound's file in each notification. Windows and macOS
//! keep the system's sound.
//!
//! Own sounds are copies of files the user picked, in `sounds/own/` of the data directory, kept
//! across sign-out like the wallpaper: at most `MAX_OWN`, each at most `MAX_OWN_BYTES`.

use std::path::Path;

use crate::i18n::t;

/// The longest list of own sounds, and the largest file one may be: a notification sound is short.
pub(crate) const MAX_OWN: usize = 16;
const MAX_OWN_BYTES: u64 = 1024 * 1024;
/// What Android's MediaPlayer and GStreamer's base and good plugins both play.
const OWN_KINDS: [&str; 7] = ["ogg", "oga", "opus", "mp3", "wav", "m4a", "flac"];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Tone {
    Dew,
    Pebble,
    Chime,
    Ripple,
    Bloom,
    Pop,
    None,
    /// One of the user's own sounds, by its file's name in `sounds/own/`.
    Own(String),
}

impl Tone {
    pub(crate) const BUILT_IN: [Self; 7] = [Self::Dew, Self::Pebble, Self::Chime, Self::Ripple, Self::Bloom, Self::Pop, Self::None];

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Dew => t!("Dew").into(),
            Self::Pebble => t!("Pebble").into(),
            Self::Chime => t!("Chime").into(),
            Self::Ripple => t!("Ripple").into(),
            Self::Bloom => t!("Bloom").into(),
            Self::Pop => t!("Pop").into(),
            Self::None => t!("None|sound").into(),
            Self::Own(file) => Path::new(file).file_stem().map_or_else(|| file.clone(), |s| s.to_string_lossy().into_owned()),
        }
    }

    /// The sound's name to `Notify.java`: a tone's in `res/raw/tone_<name>.ogg`, or `own:<file>`.
    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn name(&self) -> String {
        match self {
            Self::Dew => "dew".into(),
            Self::Pebble => "pebble".into(),
            Self::Chime => "chime".into(),
            Self::Ripple => "ripple".into(),
            Self::Bloom => "bloom".into(),
            Self::Pop => "pop".into(),
            Self::None => "none".into(),
            Self::Own(file) => format!("own:{file}"),
        }
    }

    #[cfg(target_os = "linux")]
    fn bytes(&self) -> &'static [u8] {
        match self {
            Self::Dew => include_bytes!("../android/app/src/main/res/raw/tone_dew.ogg"),
            Self::Pebble => include_bytes!("../android/app/src/main/res/raw/tone_pebble.ogg"),
            Self::Chime => include_bytes!("../android/app/src/main/res/raw/tone_chime.ogg"),
            Self::Ripple => include_bytes!("../android/app/src/main/res/raw/tone_ripple.ogg"),
            Self::Bloom => include_bytes!("../android/app/src/main/res/raw/tone_bloom.ogg"),
            Self::Pop => include_bytes!("../android/app/src/main/res/raw/tone_pop.ogg"),
            Self::None | Self::Own(_) => &[],
        }
    }
}

/// The kinds of chat that each have their own sound, as Android's channels split them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Direct,
    Group,
    Mention,
}

impl Kind {
    pub(crate) const ALL: [Self; 3] = [Self::Direct, Self::Group, Self::Mention];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Direct => t!("Direct messages"),
            Self::Group => t!("Group chats"),
            Self::Mention => t!("Mentions"),
        }
    }

    /// A mention sounds as one in any chat. Android's `Notify.java` picks the channel itself.
    #[cfg(not(target_os = "android"))]
    pub(crate) fn of(group: bool, mention: bool) -> Self {
        if mention {
            Self::Mention
        } else if group {
            Self::Group
        } else {
            Self::Direct
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Sounds {
    pub direct: Tone,
    pub group: Tone,
    pub mention: Tone,
    /// The user's own sounds, by file name in `sounds/own/`, in the order added.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub own: Vec<String>,
}

impl Default for Sounds {
    fn default() -> Self {
        Self { direct: Tone::Dew, group: Tone::Pebble, mention: Tone::Chime, own: Vec::new() }
    }
}

static CURRENT: std::sync::Mutex<Sounds> = std::sync::Mutex::new(Sounds { direct: Tone::Dew, group: Tone::Pebble, mention: Tone::Chime, own: Vec::new() });

impl Sounds {
    pub(crate) fn get(&self, kind: Kind) -> &Tone {
        match kind {
            Kind::Direct => &self.direct,
            Kind::Group => &self.group,
            Kind::Mention => &self.mention,
        }
    }

    pub(crate) fn set(&mut self, kind: Kind, tone: Tone) {
        match kind {
            Kind::Direct => self.direct = tone,
            Kind::Group => self.group = tone,
            Kind::Mention => self.mention = tone,
        }
    }

    /// Makes these the sounds notifications play; Android makes a new channel for a kind whose
    /// sound changed, with the user's other settings of the old one.
    pub(crate) fn apply(&self) {
        *CURRENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.clone();
        #[cfg(target_os = "android")]
        if let Err(e) = crate::android_files::notify("sounds", &[&self.direct.name(), &self.group.name(), &self.mention.name()]) {
            log::warn!("notification sounds: {e}");
        }
    }

    /// Copies a picked file in as an own sound and returns it. Refused, with the reason, past
    /// `MAX_OWN`, over `MAX_OWN_BYTES`, or for a file not of `OWN_KINDS`.
    pub(crate) fn add_own(&mut self, dir: &Path, picked: &Path) -> Result<Tone, String> {
        if self.own.len() >= MAX_OWN {
            return Err(t!("At most {n} own sounds. Remove one first.", n = MAX_OWN));
        }
        let ext = picked.extension().map(|e| e.to_string_lossy().to_lowercase()).filter(|e| OWN_KINDS.contains(&e.as_str()));
        let Some(ext) = ext else { return Err(t!("Choose an audio file: Ogg, MP3, WAV, M4A or FLAC.").into()) };
        let size = std::fs::metadata(picked).map_err(|e| e.kind().to_string())?.len();
        if size > MAX_OWN_BYTES {
            return Err(t!("The sound is larger than 1 MB.").into());
        }
        let stem: String = picked.file_stem().map(|s| s.to_string_lossy().chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_')).take(40).collect()).unwrap_or_default();
        let stem = if stem.trim().is_empty() { "sound".to_owned() } else { stem.trim().to_owned() };
        let file = (1..=MAX_OWN + 1)
            .map(|n| if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} {n}.{ext}") })
            .find(|f| !self.own.iter().any(|o| o.eq_ignore_ascii_case(f)))
            .unwrap_or_else(|| format!("{stem}.{ext}"));
        let folder = own_dir(dir);
        std::fs::create_dir_all(&folder).and_then(|()| std::fs::copy(picked, folder.join(&file))).map_err(|e| e.kind().to_string())?;
        self.own.push(file.clone());
        Ok(Tone::Own(file))
    }

    /// Deletes an own sound; a kind that played it plays its default again.
    pub(crate) fn remove_own(&mut self, dir: &Path, file: &str) {
        self.own.retain(|o| o != file);
        if valid(file) {
            let _ = std::fs::remove_file(own_dir(dir).join(file));
        }
        let gone = Tone::Own(file.to_owned());
        let defaults = Self::default();
        for kind in Kind::ALL {
            if *self.get(kind) == gone {
                self.set(kind, defaults.get(kind).clone());
            }
        }
    }
}

fn own_dir(dir: &Path) -> std::path::PathBuf {
    dir.join("sounds").join("own")
}

/// A file name `add_own` could have made: one hand-edited into settings.json reaches no other file.
fn valid(file: &str) -> bool {
    !file.is_empty() && !file.starts_with('.') && !file.contains(['/', '\\'])
}

/// The sound a kind of chat plays now.
#[cfg(target_os = "linux")]
pub(crate) fn current(kind: Kind) -> Tone {
    CURRENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(kind).clone()
}

/// The data directory's `sounds/`, once the tones were written there.
#[cfg(target_os = "linux")]
static FOLDER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Writes every tone to `sounds/` in the data directory (60 KB) for the notification server to
/// play by path; the file of older builds there goes.
#[cfg(target_os = "linux")]
pub(crate) fn keep(dir: &Path) {
    let folder = dir.join("sounds");
    let _ = std::fs::remove_file(folder.join("message.ogg"));
    let written = std::fs::create_dir_all(&folder)
        .and_then(|()| Tone::BUILT_IN.iter().filter(|t| **t != Tone::None).try_for_each(|t| std::fs::write(folder.join(format!("{}.ogg", t.name())), t.bytes())));
    match written {
        Ok(()) => drop(FOLDER.set(folder)),
        Err(e) => log::warn!("notification sounds: {}", e.kind()),
    }
}

/// The sound's file; `None` for no sound, before `keep`, or an own sound whose file is gone.
#[cfg(target_os = "linux")]
pub(crate) fn file(tone: &Tone) -> Option<std::path::PathBuf> {
    let folder = FOLDER.get()?;
    match tone {
        Tone::None => None,
        Tone::Own(file) => Some(folder.join("own").join(file)).filter(|p| valid(file) && p.is_file()),
        tone => Some(folder.join(format!("{}.ogg", tone.name()))),
    }
}

/// Plays a sound once, as its picker offers it: Android through the notification stream, Linux
/// through GStreamer, each stopping the one before.
pub(crate) fn preview(tone: &Tone) {
    #[cfg(target_os = "android")]
    if let Err(e) = crate::android_files::notify("preview", &[&tone.name()]) {
        log::warn!("sound preview: {e}");
    }
    #[cfg(target_os = "linux")]
    if let Some(path) = file(tone) {
        // A thread per tap, each gone within 100 ms of the next tap or 3 s.
        std::thread::spawn(move || {
            if let Err(e) = play(&path) {
                log::warn!("sound preview: {e}");
            }
        });
    }
    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    let _ = tone;
}

#[cfg(target_os = "linux")]
fn play(path: &Path) -> Result<(), String> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static LATEST: AtomicU64 = AtomicU64::new(0);
    let mine = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
    gst::init().map_err(|e| e.to_string())?;
    let uri = gstreamer::glib::filename_to_uri(path, None).map_err(|e| e.to_string())?;
    let player = gst::ElementFactory::make("playbin").property("uri", uri).build().map_err(|e| e.to_string())?;
    let bus = player.bus().ok_or("no bus")?;
    player.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
    for _ in 0..30 {
        if LATEST.load(Ordering::SeqCst) != mine || bus.timed_pop_filtered(gst::ClockTime::from_mseconds(100), &[gst::MessageType::Eos, gst::MessageType::Error]).is_some() {
            break;
        }
    }
    let _ = player.set_state(gst::State::Null);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sounds_keep_their_names_on_disk() {
        let json = serde_json::to_string(&Sounds { direct: Tone::None, mention: Tone::Own("Ding.mp3".into()), own: vec!["Ding.mp3".into()], ..Default::default() }).unwrap();
        assert_eq!(json, r#"{"direct":"none","group":"pebble","mention":{"own":"Ding.mp3"},"own":["Ding.mp3"]}"#);
        let read: Sounds = serde_json::from_str(r#"{"group":"pop"}"#).unwrap();
        assert_eq!(read, Sounds { group: Tone::Pop, ..Default::default() });
        assert_eq!(Kind::of(true, true), Kind::Mention);
        assert_eq!(Kind::of(true, false), Kind::Group);
    }

    #[test]
    fn own_sounds_are_checked_copied_and_removed() {
        let root = std::env::temp_dir().join(format!("unisend-own-sounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = Scratch(root);
        let picked = dir.path().join("My ding!.MP3");
        std::fs::write(&picked, b"ID3").unwrap();
        let mut sounds = Sounds::default();
        let tone = sounds.add_own(dir.path(), &picked).unwrap();
        assert_eq!(tone, Tone::Own("My ding.mp3".into()));
        assert_eq!(tone.label(), "My ding");
        assert_eq!(sounds.add_own(dir.path(), &picked).unwrap(), Tone::Own("My ding 2.mp3".into()));
        assert!(dir.path().join("sounds/own/My ding 2.mp3").is_file());

        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"x").unwrap();
        assert!(sounds.add_own(dir.path(), &text).is_err());
        let big = dir.path().join("big.ogg");
        std::fs::write(&big, vec![0; MAX_OWN_BYTES as usize + 1]).unwrap();
        assert!(sounds.add_own(dir.path(), &big).is_err());

        sounds.set(Kind::Group, tone.clone());
        sounds.remove_own(dir.path(), "My ding.mp3");
        assert_eq!(sounds.group, Tone::Pebble);
        assert_eq!(sounds.own, vec!["My ding 2.mp3".to_owned()]);
        assert!(!dir.path().join("sounds/own/My ding.mp3").exists());
        sounds.remove_own(dir.path(), "../settings.json");
        assert!(!valid("../x") && !valid(".hidden") && valid("a b.ogg"));
    }

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
