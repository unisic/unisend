//! Up to three Matrix accounts signed in side by side, each with its own backend. The first keeps
//! the data directory itself, as an install with one account always had it; the others live in
//! `accounts/1` and `accounts/2` under it. Settings that belong to the device stay in the first.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Accounts signed in at once: each costs a backend thread with its sync, store and caches.
pub(crate) const MAX: usize = 3;

const SUB: &str = "accounts";

/// The account the window shows; the backends read it to notify of the others while it is in front.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn active() -> usize {
    ACTIVE.load(Ordering::Relaxed)
}

pub(crate) fn set_active(slot: usize) {
    ACTIVE.store(slot, Ordering::Relaxed);
}

/// The data directory of the account in `slot`.
pub(crate) fn dir(base: &Path, slot: usize) -> PathBuf {
    if slot == 0 { base.to_owned() } else { base.join(SUB).join(slot.to_string()) }
}

/// The slot whose directory `dir` is; any directory that is not an added account's is the first.
pub(crate) fn slot_of(dir: &Path) -> usize {
    let under = dir.parent().and_then(Path::file_name).is_some_and(|n| n == SUB);
    match dir.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<usize>().ok()) {
        Some(n) if under && (1..MAX).contains(&n) => n,
        _ => 0,
    }
}

/// The added accounts' slots found on disk; the first account always exists.
pub(crate) fn added(base: &Path) -> Vec<usize> {
    (1..MAX).filter(|&slot| dir(base, slot).is_dir()).collect()
}

/// What a notification, its buttons and a share target name a chat by: the room ID, after the
/// slot's digit for an added account, so the first account's keys are what they were before.
pub(crate) fn key(slot: usize, room: &str) -> String {
    if slot == 0 { room.to_owned() } else { format!("{slot}{room}") }
}

/// The slot and the room ID (or the test chat) a `key` names.
pub(crate) fn split(key: &str) -> (usize, &str) {
    match key.as_bytes().first() {
        Some(&d) if d.is_ascii_digit() && (1..MAX).contains(&usize::from(d - b'0')) => (usize::from(d - b'0'), &key[1..]),
        _ => (0, key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_the_account_and_keep_the_first_ones_as_they_were() {
        assert_eq!(key(0, "!a:x"), "!a:x");
        assert_eq!(key(2, "!a:x"), "2!a:x");
        for slot in 0..MAX {
            assert_eq!(split(&key(slot, "!a:x")), (slot, "!a:x"));
            assert_eq!(split(&key(slot, "test")), (slot, "test"));
        }
        assert_eq!(split("3!a:x"), (0, "3!a:x"), "no fourth account");
        assert_eq!(split(""), (0, ""));
    }

    #[test]
    fn directories_map_to_slots_and_back() {
        let base = Path::new("/data/beeper-gpui");
        assert_eq!(dir(base, 0), base);
        for slot in 0..MAX {
            assert_eq!(slot_of(&dir(base, slot)), slot);
        }
        assert_eq!(slot_of(&base.join("1")), 0, "only directories under accounts/ are added ones");
        assert_eq!(slot_of(&base.join(SUB).join("7")), 0);
    }
}
