//! Updates of the Android app, outside any app store. A manifest at a URL fixed at build time
//! (`UNISEND_UPDATE_URL`, https only) names the newest release; without one nothing here runs and
//! Settings shows no updates. Every request is a plain GET without cookies, identifiers or a user
//! agent. The APK is streamed to the app's files, checked against the manifest's SHA-256, then
//! handed to Android's PackageInstaller, which also refuses one signed with another key. The same
//! checked download fetches the emoji fonts Settings offers (`emoji::SETS`).
//!
//! The manifest, e.g. a release asset at
//! `https://github.com/<owner>/<repo>/releases/latest/download/update.json`, or on any static host:
//! `{ "versionCode": 42, "versionName": "0.2.0", "url": "https://…/unisend-0.2.0.apk",
//! "sha256": "<64 hex>", "notes": "What changed" }`. `scripts/release.sh` writes it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use matrix_sdk::reqwest;
use serde::{Deserialize, Serialize};

const URL: Option<&str> = option_env!("UNISEND_UPDATE_URL");
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the next piece of the APK; the whole download may take longer.
const STALL: Duration = Duration::from_secs(60);
/// Largest manifest read; a real one is a few hundred bytes.
const MAX_MANIFEST: usize = 16 * 1024;
/// Largest APK accepted; a debug build is about 170 MB.
const MAX_APK: u64 = 400 << 20;
const MAX_NOTES: usize = 2000;
/// At most one automatic check in this many seconds.
const EVERY: u64 = 24 * 60 * 60;

/// The developer build (UNISEND_DEV=1 at compile time): a separate app that must never be offered the stable release.
pub(crate) const DEV: bool = option_env!("UNISEND_DEV").is_some();

/// Whether this build looks for updates: Android, with a manifest URL, and not the developer build.
pub(crate) fn enabled() -> bool {
    cfg!(target_os = "android") && !DEV && manifest_url().is_some()
}

fn manifest_url() -> Option<&'static str> {
    URL.filter(|u| https(u))
}

/// Where the manifest lives, for the user to read: its host.
pub(crate) fn host() -> String {
    manifest_url().and_then(|u| reqwest::Url::parse(u).ok()).and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default()
}

fn https(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()))
}

/// A release as the manifest names it, checked by `parse`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Release {
    pub version_code: u64,
    pub version_name: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: String,
}

pub(crate) fn parse(raw: &[u8]) -> Result<Release> {
    let mut release: Release = serde_json::from_slice(raw)?;
    ensure!(release.version_code > 0, "the update has no version code");
    let name = release.version_name.trim();
    ensure!(!name.is_empty() && name.len() <= 32 && !name.contains(char::is_control), "the update has no usable version name");
    release.version_name = name.to_owned();
    ensure!(https(&release.url), "the update is not offered over https");
    ensure!(release.sha256.len() == 64 && release.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "the update has no valid SHA-256");
    release.sha256.make_ascii_lowercase();
    release.notes = release.notes.trim().chars().take(MAX_NOTES).collect();
    Ok(release)
}

/// The user's choice and when the app last looked, kept beside the settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Prefs {
    /// Off until the user turns it on: nothing leaves the device for updates without a tap.
    #[serde(default)]
    pub auto: bool,
    /// Unix seconds of the last check, automatic or not.
    #[serde(default)]
    pub last_check: u64,
}

impl Prefs {
    fn path(dir: &Path) -> PathBuf {
        dir.join("update.json")
    }

    pub fn load(dir: &Path) -> Self {
        std::fs::read(Self::path(dir)).ok().and_then(|raw| serde_json::from_slice(&raw).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        let written = serde_json::to_vec(self).map_err(std::io::Error::other).and_then(|raw| std::fs::write(Self::path(dir), raw));
        if let Err(e) = written {
            log::warn!("update settings: {e}");
        }
    }

    /// An automatic check is due: on, and never checked or a day since. A clock set back counts as due.
    pub fn due(&self, now: u64) -> bool {
        self.auto && (self.last_check == 0 || now < self.last_check || now - self.last_check >= EVERY)
    }
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// What the update did, for the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// The installed version is the newest.
    Latest,
    Found(Release),
    CheckFailed(String),
    /// Percent of the APK downloaded.
    Progress(u8),
    /// The system has the APK; it may ask the user to confirm.
    Installing,
    Failed(String),
}

/// Where downloads go; emptied whenever the backend starts.
pub(super) fn folder(dir: &Path) -> PathBuf {
    dir.join("update")
}

fn without_url(e: reqwest::Error) -> reqwest::Error {
    e.without_url()
}

/// The response, refused when redirects ended anywhere but https (or this machine, for tests).
async fn get(http: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    let response = http.get(url).send().await.map_err(without_url)?.error_for_status().map_err(without_url)?;
    let end = response.url();
    ensure!(end.scheme() == "https" || end.host_str().is_some_and(super::is_local), "the download was redirected off https");
    Ok(response)
}

/// The release the manifest names when it is newer than `installed`, else `None`.
pub(super) async fn check(http: &reqwest::Client, url: &str, installed: u64) -> Result<Option<Release>> {
    let mut response = tokio::time::timeout(MANIFEST_TIMEOUT, get(http, url)).await??;
    let mut body = Vec::new();
    while let Some(chunk) = tokio::time::timeout(MANIFEST_TIMEOUT, response.chunk()).await?.map_err(without_url)? {
        ensure!(body.len() + chunk.len() <= MAX_MANIFEST, "the update manifest is too large");
        body.extend_from_slice(&chunk);
    }
    let release = parse(&body)?;
    Ok((release.version_code > installed).then_some(release))
}

/// Streams the APK to `to` and checks its SHA-256; `to` is removed when anything fails.
pub(super) async fn download(http: &reqwest::Client, release: &Release, to: &Path, progress: impl FnMut(u8)) -> Result<()> {
    file(http, &release.url, &release.sha256, MAX_APK, to, progress).await
}

/// Streams `url`, at most `max` bytes, to `to` and checks it against `sha256` (lowercase hex);
/// `to` is removed when anything fails.
pub(super) async fn file(http: &reqwest::Client, url: &str, sha256: &str, max: u64, to: &Path, progress: impl FnMut(u8)) -> Result<()> {
    let done = fetch(http, url, sha256, max, to, progress).await;
    if done.is_err() {
        let _ = std::fs::remove_file(to);
    }
    done
}

async fn fetch(http: &reqwest::Client, url: &str, sha256: &str, max: u64, to: &Path, mut progress: impl FnMut(u8)) -> Result<()> {
    let mut response = tokio::time::timeout(STALL, get(http, url)).await??;
    let total = response.content_length();
    if total.is_some_and(|n| n > max) {
        bail!("the file is too large");
    }
    // Small blocking writes on the runtime: each is one network chunk into the page cache.
    let mut file = std::fs::File::create(to)?;
    let mut hash = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    let (mut got, mut shown) = (0u64, None);
    while let Some(chunk) = tokio::time::timeout(STALL, response.chunk()).await?.map_err(without_url)? {
        got += chunk.len() as u64;
        ensure!(got <= max, "the file is too large");
        hash.update(&chunk);
        file.write_all(&chunk)?;
        if let Some(total) = total.filter(|&n| n > 0) {
            let percent = (got * 100 / total).min(100) as u8;
            if shown != Some(percent) {
                shown = Some(percent);
                progress(percent);
            }
        }
    }
    file.sync_all()?;
    let sum: String = hash.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    ensure!(sum == sha256, "the download does not match its SHA-256");
    Ok(())
}

/// Checks for a newer release than the one installed, as `Ev::Update`.
pub(super) async fn run_check(http: reqwest::Client, ev: tokio::sync::mpsc::Sender<super::Ev>) {
    let step = match (manifest_url(), installed()) {
        (Some(url), Some(installed)) => match check(&http, url, installed).await {
            Ok(Some(release)) => Step::Found(release),
            Ok(None) => Step::Latest,
            Err(e) => Step::CheckFailed(format!("{e:#}")),
        },
        _ => Step::CheckFailed("updates are not available in this build".into()),
    };
    let _ = ev.send(super::Ev::Update(step)).await;
}

/// Downloads `release`, checks it and gives it to the system to install; the file goes either way.
pub(super) async fn run_install(http: reqwest::Client, ev: tokio::sync::mpsc::Sender<super::Ev>, dir: PathBuf, release: Release) {
    // A name per download: an aborted one may still write a chunk before it stops.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let folder = folder(&dir);
    let apk = folder.join(format!("unisend-{}.apk", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    // Progress may be dropped when the UI lags; the outcome below never is.
    let progress = |p| {
        let _ = ev.try_send(super::Ev::Update(Step::Progress(p)));
    };
    let step = match std::fs::create_dir_all(&folder) {
        Err(e) => Step::Failed(e.to_string()),
        Ok(()) => match download(&http, &release, &apk, progress).await {
            Err(e) => Step::Failed(format!("{e:#}")),
            Ok(()) => {
                // The session keeps its own copy, so the download is not needed after this.
                let given = java::install(&apk);
                let _ = std::fs::remove_file(&apk);
                match given {
                    Ok(()) => Step::Installing,
                    Err(e) => Step::Failed(e),
                }
            }
        },
    };
    let _ = ev.send(super::Ev::Update(step)).await;
}

/// What a PackageInstaller status code means, for the user.
#[cfg(target_os = "android")]
fn status_text(status: i32, message: &str) -> String {
    use crate::i18n::t;
    match status {
        3 => t!("canceled").to_owned(),
        // STATUS_FAILURE_CONFLICT: most often an APK signed with another key.
        5 => t!("signed with a different key than the installed app").to_owned(),
        4 | 7 => t!("Android cannot install this file on this phone").to_owned(),
        6 => t!("not enough storage").to_owned(),
        _ if message.is_empty() => t!("Android did not install it").to_owned(),
        _ => message.to_owned(),
    }
}

/// The installed version code; `None` off Android or when it cannot be read.
fn installed() -> Option<u64> {
    java::version_code()
}

pub(crate) use java::{allow_install, can_install};

#[cfg(target_os = "android")]
mod java {
    use std::ffi::c_void;
    use std::path::Path;

    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JObject, JValue};

    const CLASS: &str = "dev.gpui.beeper.Update";

    pub(super) fn version_code() -> Option<u64> {
        let code = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("versionCode"), jni::jni_sig!("(Landroid/app/Activity;)J"), &[JValue::Object(&activity)])
                .and_then(|v| v.j())
                .map_err(|e| crate::java_error(env, e))
        });
        code.map_err(|e| log::warn!("version code: {e}")).ok().and_then(|c| u64::try_from(c).ok()).filter(|&c| c > 0)
    }

    pub(crate) fn can_install() -> bool {
        let can = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("canInstall"), jni::jni_sig!("(Landroid/app/Activity;)Z"), &[JValue::Object(&activity)])
                .and_then(|v| v.z())
                .map_err(|e| crate::java_error(env, e))
        });
        can.map_err(|e| log::warn!("can install: {e}")).unwrap_or(false)
    }

    pub(crate) fn allow_install() {
        let opened = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("allowInstall"), jni::jni_sig!("(Landroid/app/Activity;)V"), &[JValue::Object(&activity)])
                .map(|_| ())
                .map_err(|e| crate::java_error(env, e))
        });
        if let Err(e) = opened {
            log::warn!("allow install: {e}");
        }
    }

    pub(super) fn install(apk: &Path) -> Result<(), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let path = env.new_string(apk.to_string_lossy()).map_err(|e| e.to_string())?;
            let why = env
                .call_static_method(
                    &class,
                    jni::jni_str!("install"),
                    jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)Ljava/lang/String;"),
                    &[JValue::Object(&activity), JValue::Object(&path)],
                )
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            if why.is_null() { Ok(()) } else { Err(helpers::get_string(env, &why)) }
        })
    }

    /// # Safety
    /// Called by the JVM with a valid string reference.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_beeper_Update_nativeFailed(_env: *mut c_void, _class: *mut c_void, status: i32, message: *mut c_void) {
        let Ok(message) = helpers::with_env(|env| {
            let obj = unsafe { JObject::from_raw(env, message as jni::sys::jobject) };
            Ok(helpers::get_string(env, &obj))
        }) else {
            return;
        };
        super::super::update_failed(super::status_text(status, &message));
    }
}

#[cfg(not(target_os = "android"))]
mod java {
    use std::path::Path;

    pub(super) fn version_code() -> Option<u64> {
        None
    }

    pub(crate) fn can_install() -> bool {
        false
    }

    pub(crate) fn allow_install() {}

    pub(super) fn install(_: &Path) -> Result<(), String> {
        Err("installing updates needs Android".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUM: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn manifest(url: &str, sum: &str) -> String {
        format!(r#"{{"versionCode": 7, "versionName": " 0.2.0 ", "url": "{url}", "sha256": "{sum}", "notes": " Faster. "}}"#)
    }

    #[test]
    fn a_manifest_is_checked_before_it_is_believed() {
        let ok = parse(manifest("https://example.org/m.apk", &SUM.to_uppercase()).as_bytes()).unwrap();
        assert_eq!(ok, Release { version_code: 7, version_name: "0.2.0".into(), url: "https://example.org/m.apk".into(), sha256: SUM.into(), notes: "Faster.".into() });
        assert!(parse(manifest("http://example.org/m.apk", SUM).as_bytes()).is_err(), "plain http");
        assert!(parse(manifest("file:///sdcard/m.apk", SUM).as_bytes()).is_err());
        assert!(parse(manifest("https://", SUM).as_bytes()).is_err(), "no host");
        assert!(parse(manifest("https://example.org/m.apk", &SUM[1..]).as_bytes()).is_err(), "short sum");
        assert!(parse(manifest("https://example.org/m.apk", &SUM.replace('9', "g")).as_bytes()).is_err(), "not hex");
        assert!(parse(br#"{"versionCode": 0, "versionName": "1", "url": "https://a.b/c", "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}"#).is_err());
        assert!(parse(br#"{"versionCode": -1, "versionName": "1", "url": "https://a.b/c", "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}"#).is_err());
        assert!(parse(br#"{"versionCode": 3, "versionName": "", "url": "https://a.b/c", "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}"#).is_err());
        let long = format!(r#"{{"versionCode": 3, "versionName": "1", "url": "https://a.b/c", "sha256": "{SUM}", "notes": "{}"}}"#, "x".repeat(5000));
        assert_eq!(parse(long.as_bytes()).unwrap().notes.len(), MAX_NOTES);
        assert!(https("https://github.com/o/r/releases/latest/download/update.json") && !https("HTTP://a.b/c") && !https("not a url"));
    }

    #[test]
    fn checks_at_most_once_a_day_and_only_when_on() {
        assert!(!Prefs::default().due(1_000), "off until turned on");
        let mut prefs = Prefs { auto: true, ..Prefs::default() };
        assert!(prefs.due(1_000), "never checked");
        prefs.last_check = 1_000;
        assert!(!prefs.due(1_000 + EVERY - 1));
        assert!(prefs.due(1_000 + EVERY));
        assert!(prefs.due(999), "clock set back");
        prefs.auto = false;
        assert!(!prefs.due(1_000 + 10 * EVERY));
        assert_eq!(serde_json::from_str::<Prefs>("{}").unwrap(), Prefs::default(), "missing fields default to off");
    }

    /// Answers each connection with `body` once, over plain http on this machine.
    fn serve(body: Vec<u8>) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body));
            }
        });
        format!("http://{addr}/unisend.apk")
    }

    #[tokio::test]
    async fn a_download_is_kept_only_when_its_hash_matches() {
        let body = b"test".repeat(50_000);
        let sum: String = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &body).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let url = serve(body.clone());
        let dir = std::env::temp_dir().join(format!("unisend-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let to = dir.join("unisend.apk");
        let http = reqwest::Client::new();
        let mut release = Release { version_code: 2, version_name: "2".into(), url, sha256: sum, notes: String::new() };
        let mut seen = Vec::new();
        download(&http, &release, &to, |p| seen.push(p)).await.unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), body);
        assert!(seen.windows(2).all(|w| w[0] < w[1]) && seen.last() == Some(&100), "{seen:?}");

        release.sha256 = SUM.into();
        let e = download(&http, &release, &to, |_| {}).await.unwrap_err();
        assert!(format!("{e:#}").contains("SHA-256"), "{e:#}");
        assert!(!to.exists(), "a file that does not match is removed");

        let manifest = serve(manifest("https://example.org/m.apk", SUM).into_bytes()).replace("unisend.apk", "update.json");
        assert_eq!(check(&http, &manifest, 6).await.unwrap().map(|r| r.version_code), Some(7));
        assert_eq!(check(&http, &manifest, 7).await.unwrap(), None, "same version is not an update");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
