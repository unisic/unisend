//! One app per data directory on desktop. A second copy would share the encryption store with the
//! first, and the two would upload one-time keys over each other: the server then refuses every
//! later key upload and messages sent to this device stay unreadable. A second launch hands the
//! first its activation token, so the first can come to front, and ends.

use std::fs::File;
#[cfg(unix)]
use std::io::{Read as _, Write as _};
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

use gpui_kit::App;

/// Bytes an activation token may take; KWin's are about 40.
#[cfg(unix)]
const MAX_TOKEN: u64 = 512;
/// Raise requests waiting for the UI; more are dropped.
#[cfg(unix)]
const QUEUE: usize = 4;

/// The directory's lock, held while the process lives (the system lets go of it when the process
/// ends, a crash too), and where later launches knock.
pub(crate) struct Claim {
    lock: Option<File>,
    #[cfg(unix)]
    door: Option<std::os::unix::net::UnixListener>,
}

/// `None` when another copy holds `dir`: it has been asked to come to front.
pub(crate) fn claim(dir: &Path) -> Option<Claim> {
    let lock = match File::options().create(true).truncate(false).write(true).open(dir.join("instance.lock")) {
        Ok(f) => f,
        Err(e) => {
            // Without the lock file the guard cannot work; running unguarded beats not running.
            log::warn!("single instance: {e}");
            return Some(Claim { lock: None, #[cfg(unix)] door: None });
        }
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            knock(dir);
            return None;
        }
        // A file system without locks: as above.
        Err(std::fs::TryLockError::Error(e)) => log::warn!("single instance: {e}"),
    }
    #[cfg(unix)]
    let door = {
        let path = dir.join("instance.sock");
        // Left by a copy that ended without removing it; the lock says none is running.
        let _ = std::fs::remove_file(&path);
        std::os::unix::net::UnixListener::bind(&path).map_err(|e| log::warn!("single instance: {e}")).ok()
    };
    Some(Claim { lock: Some(lock), #[cfg(unix)] door })
}

/// Asks the copy that holds `dir` to come to front, with this launch's activation token.
fn knock(dir: &Path) {
    eprintln!("Unisend is already running");
    #[cfg(unix)]
    {
        let token = std::env::var("XDG_ACTIVATION_TOKEN").or_else(|_| std::env::var("DESKTOP_STARTUP_ID")).unwrap_or_default();
        // The first copy may hold the lock a moment before its door is open.
        for _ in 0..20 {
            if let Ok(mut s) = std::os::unix::net::UnixStream::connect(dir.join("instance.sock")) {
                let _ = s.write_all(token.as_bytes());
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // shortcut: Windows has no door, so a second launch only ends; upgrade with a named pipe if
    // the window should come to front there too.
    #[cfg(not(unix))]
    let _ = dir;
}

/// Brings the window to front, or opens it again, whenever a later launch knocks.
pub(crate) fn serve(claim: Claim, cx: &mut App) {
    // The lock goes with the process; the claim only keeps it open until then.
    std::mem::forget(claim.lock);
    #[cfg(unix)]
    {
        let Some(door) = claim.door else { return };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(QUEUE);
        std::thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                for stream in door.incoming().flatten() {
                    // A knock that never finishes must not hold the next one up.
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let mut token = String::new();
                    let _ = stream.take(MAX_TOKEN).read_to_string(&mut token);
                    let _ = tx.try_send(token);
                }
            })
            .map_err(|e| log::warn!("single instance: {e}"))
            .ok();
        cx.spawn(async move |cx| {
            while let Some(token) = rx.recv().await {
                cx.update(|cx| {
                    #[cfg(target_os = "linux")]
                    if !token.is_empty() {
                        gpui_linux::set_next_activation_token(token);
                    }
                    #[cfg(not(target_os = "linux"))]
                    let _ = token;
                    crate::desktop::show(cx);
                });
            }
        })
        .detach();
    }
    #[cfg(not(unix))]
    let _ = cx;
}

#[cfg(test)]
mod tests {
    #[test]
    fn second_copy_is_refused_until_the_first_ends() {
        let dir = std::env::temp_dir().join(format!("beeper-gpui-instance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = super::claim(&dir);
        assert!(first.is_some());
        assert!(super::claim(&dir).is_none());
        drop(first);
        assert!(super::claim(&dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
