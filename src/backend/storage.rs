//! What the account keeps on this device, for Settings' Storage page: how much its caches take,
//! emptying the media cache, and how long messages stay in the caches here. Nothing on the server
//! changes, and the keys and the encryption store (`docs/data-encryption.md`) are never touched:
//! the media cache is emptied through the SDK, which leaves the rest of the store as it is.
//!
//! The SDK's media cache keeps its default bounds (400 MiB, 20 MiB a file, daily cleanup) and
//! drops what was not opened within the kept time, 60 days at most. Before this the store had no
//! bound at all (matrix-sdk-base 0.19.1 `MediaService::new` starts with an empty policy).
//!
//! Cost: `usage` is a `stat` of about a dozen files plus a listing of the downloads folder, which
//! `MEDIA_TTL` keeps to a few dozen entries. Emptying the cache is one SDK cleanup and `VACUUM`.

use std::path::Path;
use std::time::Duration;

use matrix_sdk::Client;
use matrix_sdk::media::MediaRetentionPolicy;

/// How long messages stay in this device's caches.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Keep {
    #[default]
    Forever,
    Year,
    Month,
}

impl Keep {
    pub const ALL: [Self; 3] = [Self::Forever, Self::Year, Self::Month];

    pub fn age(self) -> Option<Duration> {
        const DAY: u64 = 24 * 60 * 60;
        match self {
            Self::Forever => None,
            Self::Year => Some(Duration::from_secs(365 * DAY)),
            Self::Month => Some(Duration::from_secs(30 * DAY)),
        }
    }
}

/// Media not opened for this long leaves the cache whatever `Keep` says: it is a cache.
const MEDIA_EXPIRY: Duration = Duration::from_secs(60 * 24 * 60 * 60);

/// Bytes on disk, by what they hold.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Pictures, videos and files the SDK cached.
    pub media: u64,
    /// The conversations' events the SDK cached.
    pub messages: u64,
    pub search: u64,
    /// Attachments decrypted for another app or a player, gone after `MEDIA_TTL`.
    pub downloads: u64,
    /// The chat list, rooms' state, keys and the rest of the account.
    pub account: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.media + self.messages + self.search + self.downloads + self.account
    }
}

/// The SDK's databases under `store/` (matrix-sdk-sqlite 0.19.1 `DATABASE_NAME`s).
const MEDIA_DB: &str = "matrix-sdk-media.sqlite3";
const EVENTS_DB: &str = "matrix-sdk-event-cache.sqlite3";
const ACCOUNT_DBS: [&str; 2] = ["matrix-sdk-state.sqlite3", "matrix-sdk-crypto.sqlite3"];

/// What the account in `dir` takes. Blocking: a few `stat` calls.
pub(super) fn usage(dir: &Path) -> Usage {
    let size = |path: &Path| std::fs::metadata(path).map_or(0, |m| m.len());
    // A database with its write-ahead log and shared memory.
    let db = |name: &str| ["", "-wal", "-shm"].iter().map(|suffix| size(&dir.join("store").join(format!("{name}{suffix}")))).sum::<u64>();
    let search = ["search.enc", "search.sqlite3", "search.sqlite3-wal", "search.sqlite3-shm"].iter().map(|name| size(&dir.join(name))).sum();
    let downloads = folder(&super::media_dir(dir), 2);
    let files = ["session.enc", "session.json", "rooms.enc", "rooms.json", "drafts.enc", "drafts.json", "store.key"].iter().map(|name| size(&dir.join(name))).sum::<u64>();
    Usage { media: db(MEDIA_DB), messages: db(EVENTS_DB), search, downloads, account: ACCOUNT_DBS.iter().map(|name| db(name)).sum::<u64>() + files }
}

/// Bytes of the files under `dir`, `depth` levels down at most.
fn folder(dir: &Path, depth: u8) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(m) if m.is_dir() && depth > 0 => folder(&entry.path(), depth - 1),
            Ok(m) if m.is_file() => m.len(),
            _ => 0,
        })
        .sum()
}

/// The media cache's bounds for `keep`.
pub(super) fn policy(keep: Keep) -> MediaRetentionPolicy {
    let expiry = keep.age().map_or(MEDIA_EXPIRY, |age| age.min(MEDIA_EXPIRY));
    MediaRetentionPolicy::new().with_last_access_expiry(Some(expiry))
}

/// Applies `keep` to the media cache and the search index.
pub(super) async fn keep(client: &Client, keep: Keep) {
    super::search::keep(client, keep.age());
    let media = client.media();
    let wanted = policy(keep);
    match media.media_retention_policy().await {
        Ok(now) if now == wanted => {}
        _ => {
            if let Err(e) = media.set_media_retention_policy(wanted).await {
                log::warn!("media cache policy: {e}");
                return;
            }
            if let Err(e) = media.clean().await {
                log::warn!("media cache cleanup: {e}");
            }
        }
    }
}

/// Empties the media cache and the downloads folder. Media still waiting to be sent is kept: the
/// SDK stores it outside the retention policy.
pub(super) async fn clear_media(client: &Client, dir: &Path) -> anyhow::Result<()> {
    let media = client.media();
    let kept = media.media_retention_policy().await?;
    // A cache of at most 0 bytes: the cleanup removes every entry the policy governs.
    media.set_media_retention_policy(kept.with_max_cache_size(Some(0))).await?;
    let cleaned = media.clean().await;
    media.set_media_retention_policy(kept).await?;
    cleaned?;
    let downloads = super::media_dir(dir);
    tokio::task::spawn_blocking(move || super::sweep_now(&downloads)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_expires_within_the_kept_time() {
        assert_eq!(policy(Keep::Forever).last_access_expiry, Some(MEDIA_EXPIRY));
        assert_eq!(policy(Keep::Year).last_access_expiry, Some(MEDIA_EXPIRY));
        assert_eq!(policy(Keep::Month).last_access_expiry, Keep::Month.age());
        assert_eq!(policy(Keep::Month).max_cache_size, MediaRetentionPolicy::default().max_cache_size, "the cache stays bounded");
    }

    #[test]
    fn usage_counts_each_part() {
        let dir = std::env::temp_dir().join(format!("unisend-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("store")).unwrap();
        std::fs::create_dir_all(dir.join("media/$event")).unwrap();
        std::fs::write(dir.join("store").join(MEDIA_DB), [0; 10]).unwrap();
        std::fs::write(dir.join("store").join(format!("{MEDIA_DB}-wal")), [0; 5]).unwrap();
        std::fs::write(dir.join("store").join(EVENTS_DB), [0; 7]).unwrap();
        std::fs::write(dir.join("store/matrix-sdk-crypto.sqlite3"), [0; 3]).unwrap();
        std::fs::write(dir.join("search.enc"), [0; 4]).unwrap();
        std::fs::write(dir.join("media/$event/photo.jpg"), [0; 6]).unwrap();
        let usage = usage(&dir);
        assert_eq!(usage, Usage { media: 15, messages: 7, search: 4, downloads: 6, account: 3 });
        assert_eq!(usage.total(), 35);
        let _ = std::fs::remove_dir_all(dir);
    }
}
