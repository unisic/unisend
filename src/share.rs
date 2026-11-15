//! What other apps share into this one through Android's share sheet, and the recent chats
//! offered back to that sheet as Direct Share targets. Desktop has no share sheet; a debug build
//! takes one share from `BEEPER_GPUI_SHARE` (file paths, one per line) and
//! `BEEPER_GPUI_SHARE_TEXT`, only so the screen can be checked there.

use std::path::{Path, PathBuf};

/// Files taken from one share; the rest are left out and the screen says so.
pub(crate) const MAX_FILES: usize = 20;
/// Bytes copied into the cache for one share, all files together.
pub(crate) const MAX_BYTES: u64 = 200 << 20;
/// Chats offered in other apps' share sheets; the sheet shows about this many in a row.
pub(crate) const MAX_TARGETS: usize = 4;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Incoming {
    /// The chat a Direct Share target named.
    pub chat: Option<String>,
    pub text: String,
    /// Content addresses on Android, paths on desktop.
    pub uris: Vec<String>,
}

/// Parses `Share.take`'s answer: chat, text, then addresses, separated by NUL.
fn parse(raw: &str) -> Incoming {
    let mut fields = raw.split('\0');
    let chat = fields.next().filter(|c| !c.is_empty()).map(str::to_owned);
    let text = fields.next().unwrap_or_default().trim().to_owned();
    let uris = fields.filter(|u| !u.is_empty()).map(str::to_owned).collect();
    Incoming { chat, text, uris }
}

/// The share the app was opened or brought forward with, once.
pub(crate) fn take() -> Option<Incoming> {
    #[cfg(target_os = "android")]
    return jni::take().map_err(|e| log::warn!("share: {e}")).ok().flatten().map(|raw| parse(&raw));
    #[cfg(all(not(target_os = "android"), debug_assertions))]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let files = std::env::var("BEEPER_GPUI_SHARE").unwrap_or_default();
        let text = std::env::var("BEEPER_GPUI_SHARE_TEXT").unwrap_or_default();
        if (files.is_empty() && text.is_empty()) || TAKEN.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let raw = format!("\0{text}\0{}", files.lines().collect::<Vec<_>>().join("\0"));
        Some(parse(&raw))
    }
    #[cfg(all(not(target_os = "android"), not(debug_assertions)))]
    None
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Copied {
    pub paths: Vec<PathBuf>,
    /// Past `MAX_FILES`, `MAX_BYTES` or the upload limit of one file.
    pub over: usize,
    pub unreadable: usize,
}

/// Copies shared files into the app's cache, in order, within the limits. Blocks: run it off the
/// UI thread. Desktop's debug share uses the files where they are.
pub(crate) fn copy(uris: &[String]) -> Copied {
    #[cfg(target_os = "android")]
    {
        let size = |uri: &str| jni::size(uri).map_err(|e| log::warn!("shared file size: {e}")).ok().flatten();
        let copy = |uri: &str| crate::android_files::copy_to_cache(uri).map_err(|e| log::warn!("shared file: {e}")).ok().flatten().map(PathBuf::from);
        within_limits(uris, (MAX_FILES, MAX_BYTES, crate::backend::MAX_UPLOAD), size, copy, discard)
    }
    #[cfg(not(target_os = "android"))]
    within_limits(uris, (MAX_FILES, MAX_BYTES, crate::backend::MAX_UPLOAD), |uri| std::fs::metadata(uri).ok().map(|m| m.len()), |uri| Some(PathBuf::from(uri)), |_| {})
}

/// Removes copies that were never sent, each with the folder `copy_to_cache` made for it.
pub(crate) fn discard(path: &Path) {
    if cfg!(target_os = "android") {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// A file whose size its provider does not tell is copied, then measured, and dropped when it
/// does not fit; so at most one file past the budget is ever on disk at once.
fn within_limits(uris: &[String], (max_files, max_bytes, max_file): (usize, u64, u64), size: impl Fn(&str) -> Option<u64>, copy: impl Fn(&str) -> Option<PathBuf>, drop: impl Fn(&Path)) -> Copied {
    let mut out = Copied::default();
    let mut used = 0u64;
    for uri in uris {
        let fits = |n: u64| n <= max_file && used.saturating_add(n) <= max_bytes;
        if out.paths.len() == max_files || size(uri).is_some_and(|n| !fits(n)) {
            out.over += 1;
            continue;
        }
        let Some(path) = copy(uri) else {
            out.unreadable += 1;
            continue;
        };
        let Ok(n) = std::fs::metadata(&path).map(|m| m.len()) else {
            out.unreadable += 1;
            continue;
        };
        if !fits(n) {
            drop(&path);
            out.over += 1;
            continue;
        }
        used += n;
        out.paths.push(path);
    }
    out
}

/// Offers these chats, most recent first, in other apps' share sheets: ID, name, and the path of
/// the chat's picture when there is one.
pub(crate) fn publish(chats: &[(String, String, Option<PathBuf>)]) {
    #[cfg(target_os = "android")]
    {
        let mut raw = Vec::with_capacity(chats.len() * 3);
        for (id, name, icon) in chats {
            raw.push(id.replace('\0', " "));
            raw.push(name.replace('\0', " "));
            raw.push(icon.as_deref().and_then(Path::to_str).unwrap_or_default().to_owned());
        }
        if let Err(e) = jni::publish(&raw.join("\0")) {
            log::warn!("share targets: {e}");
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = chats;
}

/// Calls into `dev.gpui.beeper.Share`.
#[cfg(target_os = "android")]
mod jni {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    const CLASS: &str = "dev.gpui.beeper.Share";

    pub fn take() -> Result<Option<String>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let raw = env
                .call_static_method(&class, jni::jni_str!("take"), jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"), &[JValue::Object(&activity)])
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            Ok((!raw.is_null()).then(|| helpers::get_string(env, &raw)))
        })
    }

    pub fn size(uri: &str) -> Result<Option<u64>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let uri = env.new_string(uri).map_err(|e| e.to_string())?;
            let size = env
                .call_static_method(
                    &class,
                    jni::jni_str!("size"),
                    jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)J"),
                    &[JValue::Object(&activity), JValue::Object(&uri)],
                )
                .and_then(|v| v.j())
                .map_err(|e| crate::java_error(env, e))?;
            Ok(u64::try_from(size).ok())
        })
    }

    pub fn publish(chats: &str) -> Result<(), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let chats = env.new_string(chats).map_err(|e| e.to_string())?;
            env.call_static_method(
                &class,
                jni::jni_str!("publish"),
                jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)V"),
                &[JValue::Object(&activity), JValue::Object(&chats)],
            )
            .map(|_| ())
            .map_err(|e| crate::java_error(env, e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_share() {
        assert_eq!(parse("\0 hello \0content://a\0content://b"), Incoming {
            chat: None,
            text: "hello".into(),
            uris: vec!["content://a".into(), "content://b".into()]
        });
        assert_eq!(parse("!room:x\0\0"), Incoming { chat: Some("!room:x".into()), ..Default::default() });
    }

    #[test]
    fn keeps_files_within_count_and_bytes() {
        let dir = std::env::temp_dir().join(format!("beeper-gpui-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, len: usize| {
            let path = dir.join(name);
            std::fs::write(&path, vec![0u8; len]).unwrap();
            path.to_string_lossy().into_owned()
        };
        let (ten, mid, big, quiet) = (file("ten", 10), file("mid", 45), file("big", 60), file("quiet", 60));
        // The "provider" tells every size but `quiet`'s.
        let size = |uri: &str| (!uri.ends_with("quiet")).then(|| std::fs::metadata(uri).map_or(0, |m| m.len()));
        let copy = |uri: &str| (!uri.ends_with("missing")).then(|| PathBuf::from(uri));
        let dropped = std::cell::RefCell::new(Vec::new());
        let drop = |p: &Path| dropped.borrow_mut().push(p.to_owned());

        // Three files, 50 bytes in all, 40 bytes each; `mid` would fit the total but not one file's limit.
        let uris = [mid.clone(), ten.clone(), big.clone(), "x/missing".into(), ten.clone(), quiet.clone(), ten.clone(), ten.clone()];
        let out = within_limits(&uris, (3, 50, 40), size, copy, drop);
        assert_eq!(out.paths, vec![PathBuf::from(&ten); 3]);
        assert_eq!((out.over, out.unreadable), (4, 1));
        // `big` was refused by its reported size; `quiet` had to be copied to be measured.
        assert_eq!(*dropped.borrow(), vec![PathBuf::from(&quiet)]);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
