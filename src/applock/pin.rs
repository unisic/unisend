//! The app PIN. Only a salted PBKDF2-HMAC-SHA256 of it is kept, in the app's private data
//! directory, together with the run of wrong tries, so killing the app does not reset the wait.
//! It keeps someone holding the unlocked phone out of Murmur; it does not stand up to someone who
//! can read the app's files (root, a forensic copy): four to eight digits are quickly guessed
//! offline, however slow the hash.

use std::fs;
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::{pbkdf2, rand};

const FILE: &str = "applock-pin.json";
const TEMP: &str = "applock-pin.json.tmp";
/// OWASP's figure for PBKDF2-HMAC-SHA256 (Password Storage Cheat Sheet, 2023).
pub(crate) const ITERATIONS: u32 = 600_000;
pub(crate) const MIN_LEN: usize = 4;
pub(crate) const MAX_LEN: usize = 8;
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;
/// Wrong PINs in a row that cost nothing; each one after that doubles the wait.
const FREE_TRIES: u32 = 4;
const FIRST_WAIT: u64 = 30;
pub(crate) const MAX_WAIT: u64 = 3600;

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    salt: Vec<u8>,
    iterations: u32,
    hash: Vec<u8>,
    #[serde(default)]
    failures: u32,
    /// Unix seconds before which no PIN is checked.
    #[serde(default)]
    retry_at: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Check {
    Right,
    /// Seconds before the next try.
    Wrong(u64),
    /// Not checked: still waiting this many seconds.
    Wait(u64),
    /// No PIN is set, or the file is not one this code wrote.
    Missing,
}

pub(crate) fn valid(pin: &str) -> bool {
    (MIN_LEN..=MAX_LEN).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit())
}

/// Seconds to wait after `failures` wrong PINs in a row: none for the first four, then 30 s
/// doubling to an hour.
pub(crate) fn delay(failures: u32) -> u64 {
    match failures.checked_sub(FREE_TRIES + 1) {
        None => 0,
        Some(over) => FIRST_WAIT.saturating_mul(1 << over.min(16)).min(MAX_WAIT),
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub(crate) fn set(dir: &Path, pin: &str) -> io::Result<()> {
    set_with(dir, pin, ITERATIONS)
}

fn set_with(dir: &Path, pin: &str, iterations: u32) -> io::Result<()> {
    if !valid(pin) {
        return Err(io::Error::other("a PIN has 4 to 8 digits"));
    }
    let rounds = NonZeroU32::new(iterations).ok_or_else(|| io::Error::other("no iterations"))?;
    let mut salt = vec![0; SALT_LEN];
    rand::fill(&mut salt).map_err(|_| io::Error::other("no random bytes for the salt"))?;
    let mut hash = vec![0; HASH_LEN];
    pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA256, rounds, &salt, pin.as_bytes(), &mut hash);
    save(dir, &Record { salt, iterations, hash, failures: 0, retry_at: 0 })
}

pub(crate) fn clear(dir: &Path) {
    match fs::remove_file(dir.join(FILE)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => log::warn!("app PIN: {e}"),
        _ => {}
    }
}

/// Seconds before a PIN may be tried, as the lock screen opens.
pub(crate) fn wait(dir: &Path, now: u64) -> u64 {
    load(dir, now).map_or(0, |r| r.retry_at.saturating_sub(now))
}

/// Checks `pin` and counts a wrong one. Runs the hash: about 0.1 to 0.5 s, off the UI thread.
pub(crate) fn check(dir: &Path, pin: &str, now: u64) -> Check {
    let Some(mut r) = load(dir, now) else {
        return Check::Missing;
    };
    let left = r.retry_at.saturating_sub(now);
    if left > 0 {
        return Check::Wait(left);
    }
    let Some(rounds) = NonZeroU32::new(r.iterations) else {
        return Check::Missing;
    };
    // Compares in constant time.
    let right = pbkdf2::verify(pbkdf2::PBKDF2_HMAC_SHA256, rounds, &r.salt, pin.as_bytes(), &r.hash).is_ok();
    if right {
        if r.failures != 0 || r.retry_at != 0 {
            (r.failures, r.retry_at) = (0, 0);
            if let Err(e) = save(dir, &r) {
                log::warn!("app PIN: {e}");
            }
        }
        return Check::Right;
    }
    r.failures = r.failures.saturating_add(1);
    let wait = delay(r.failures);
    r.retry_at = now + wait;
    if let Err(e) = save(dir, &r) {
        // The try still counts as wrong; only the wait is lost.
        log::warn!("app PIN: {e}");
    }
    Check::Wrong(wait)
}

/// The record, with a wait that a clock set back would stretch past `MAX_WAIT` cut to it.
fn load(dir: &Path, now: u64) -> Option<Record> {
    let raw = fs::read(dir.join(FILE)).ok()?;
    let mut r: Record = serde_json::from_slice(&raw).inspect_err(|e| log::warn!("app PIN: {e}")).ok()?;
    if r.salt.is_empty() || r.hash.len() != HASH_LEN {
        log::warn!("app PIN: unexpected record");
        return None;
    }
    if r.retry_at > now.saturating_add(MAX_WAIT) {
        r.retry_at = now + MAX_WAIT;
        if let Err(e) = save(dir, &r) {
            log::warn!("app PIN: {e}");
        }
    }
    Some(r)
}

/// Written to a temporary file, synced, then renamed over the old one: a crash leaves one or the other.
fn save(dir: &Path, r: &Record) -> io::Result<()> {
    let raw = serde_json::to_vec(r).map_err(io::Error::other)?;
    let temp = dir.join(TEMP);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temp)?;
    file.write_all(&raw)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, dir.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("murmur-pin-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_four_to_eight_digits() {
        assert!(valid("0000") && valid("12345678"));
        assert!(!valid("123") && !valid("123456789") && !valid("12a4") && !valid("١٢٣٤") && !valid(""));
    }

    #[test]
    fn waits_grow_after_the_fifth_wrong_pin() {
        let waits: Vec<u64> = (0..=14).map(delay).collect();
        assert_eq!(waits, [0, 0, 0, 0, 0, 30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
        assert_eq!(delay(u32::MAX), MAX_WAIT);
    }

    #[test]
    fn the_pin_is_not_stored_and_is_checked() {
        let dir = dir("check");
        set(&dir, "4711").unwrap();
        let raw = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!raw.contains("4711"));
        assert_eq!(check(&dir, "4711", 1000), Check::Right);
        assert_eq!(check(&dir, "4712", 1000), Check::Wrong(0));
        assert_eq!(check(&dir, "47110", 1000), Check::Wrong(0));
        // Two salts, two hashes for the same PIN.
        set(&dir, "4711").unwrap();
        assert_ne!(fs::read_to_string(dir.join(FILE)).unwrap(), raw);
        assert!(set(&dir, "47").is_err());
        clear(&dir);
        assert_eq!(check(&dir, "4711", 1000), Check::Missing);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wrong_tries_survive_a_restart_and_a_right_pin_resets_them() {
        let dir = dir("limit");
        set_with(&dir, "123456", 1000).unwrap();
        let t = 10_000;
        for _ in 0..4 {
            assert_eq!(check(&dir, "000000", t), Check::Wrong(0));
        }
        assert_eq!(check(&dir, "000000", t), Check::Wrong(30));
        // Read back from the file, as after a restart: even the right PIN waits.
        assert_eq!(wait(&dir, t + 10), 20);
        assert_eq!(check(&dir, "123456", t + 10), Check::Wait(20));
        assert_eq!(check(&dir, "000000", t + 30), Check::Wrong(60));
        assert_eq!(check(&dir, "123456", t + 90), Check::Right);
        assert_eq!(check(&dir, "000000", t + 90), Check::Wrong(0));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_clock_set_back_waits_an_hour_at_most() {
        let dir = dir("clock");
        set_with(&dir, "1234", 1000).unwrap();
        let mut t = 100_000;
        for _ in 0..12 {
            match check(&dir, "9999", t) {
                Check::Wrong(wait) => t += wait,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(wait(&dir, t - MAX_WAIT), MAX_WAIT);
        assert_eq!(wait(&dir, 10_000), MAX_WAIT);
        assert_eq!(wait(&dir, 10_000 + MAX_WAIT), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_foreign_file_counts_as_no_pin() {
        let dir = dir("foreign");
        fs::write(dir.join(FILE), br#"{"salt":[1],"iterations":1000,"hash":[1,2,3]}"#).unwrap();
        assert_eq!(check(&dir, "1234", 0), Check::Missing);
        fs::write(dir.join(FILE), b"not json").unwrap();
        assert_eq!(check(&dir, "1234", 0), Check::Missing);
        fs::remove_dir_all(dir).unwrap();
    }
}
