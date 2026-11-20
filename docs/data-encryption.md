# Data at rest

How Unisend keeps what it stores on the phone. Code: `src/vault.rs`,
`android/app/src/main/java/dev/gpui/beeper/DataKey.java`, `src/backend.rs` (`unlock`, `lost`,
`restore`, `fresh_store`, `wipe_account`), `src/backend/search.rs`.

## Threat model

Protected: the app's files copied off the phone, by root access, a forensic tool or `adb run-as`
on a debuggable build. Without the phone's Keystore they are random bytes.

Not protected:

- Root, or malware with the app's rights, on the live phone. The Keystore key works for anyone
  running as the app, and the app has the store key in memory while it runs.
- A copy taken while the phone is on: the Keystore key needs neither the screen unlocked nor
  the user's credential, so that background sync works on a locked phone.
- The debuggable build: `adb run-as dev.gpui.beeper` runs as the app and can ask the Keystore to
  unwrap the key. Only a signed release build without `android:debuggable` closes that.
- Desktop: the store and files stay plain (no hardware key store is wired yet).
- Android installs signed in before this change (legacy, below).
- Short-lived plain files handed to other apps (below).

## Keys

- Store key: 32 random bytes (`aws_lc_rs::rand`), new for every sign-in, held in memory in
  `zeroize::Zeroizing`. It goes to the SDK through `SqliteStoreConfig::key`, which takes it as is.
- Wrapping key: AES-256-GCM in `AndroidKeyStore`, alias `murmur-data`, not exportable, no user
  authentication, no unlocked-device requirement; StrongBox when the phone has it, the TEE
  otherwise. `DataKey.wrap` and `DataKey.unwrap` take and return `IV (12) | ciphertext | tag`.

## Files (under the app's `files/beeper/`)

| File | Content | Form |
|---|---|---|
| `store.key` | store key wrapped by the Keystore, 0600, written with fsync | wrapped |
| `store/` | the SDK's four SQLite databases | values encrypted by the SDK |
| `session.enc` | access token and device | sealed |
| `rooms.enc` | chat list with names and message previews | sealed |
| `drafts.enc` | unsent text per chat | sealed |
| `people.enc` | nicknames and notes the user gave people (`ui/nick.rs`) | sealed |
| `search.enc` | message search index (FTS5), kept in memory, saved every 5 minutes while it changes, at most 24 MiB | sealed |
| `rooms-lang.txt`, `settings.json`, `emoji-recents.json`, `hidden-<account>.txt` (ids of hidden messages), `wallpaper.jpg`, `wallpapers/*.jpg` (a chat's own photo), `klipy-id`, app PIN hash, emoji font | no chat content | plain |

A sealed file is `1 | nonce (12, random) | AES-256-GCM ciphertext | tag (16)`. The format byte and
the plain file name are authenticated with it (AAD), so one file cannot be swapped for another and
the format cannot be changed without the tag failing. `session.enc` is written with fsync. A sealed
or chat-content file is written only while a session exists, under the same lock that sign-out
holds while it deletes, so a write racing a sign-out cannot bring a file back.

What the SDK leaves readable in `store/` (matrix-sdk-sqlite 0.19.1): table and index names, row
counts and sizes, integer columns (timestamps, send queue priority and order, chunk links), the
relation type of events (`rel_type`, such as `m.replace`), send queue transaction ids, and the
names of the crypto store's key-value entries. Room, event and user ids are stored hashed;
event contents, state, members, keys and media are encrypted. Values written with
`set_custom_value` are stored plain; only sliding sync and OAuth's cross-process refresh lock use
them, and this app uses neither.

The media cache inside `store/` is bounded: 400 MiB, 20 MiB a file, media not opened for 60 days
(or the shorter time Settings keeps messages) removed by the SDK's daily cleanup. Settings, Data and
storage empties it through the SDK; no other part of `store/` and no key is touched by that.

Plain on purpose, short-lived:

- `media/`: attachments downloaded to open in another app or to play. Each event's folder is
  deleted 10 minutes after it was written (swept on each download and every 5 minutes), and the
  whole directory at start and at sign-out. A player or app that already opened the file keeps
  reading it; one that opens it again later gets an error and the file is downloaded anew. A
  bridged GIF's clip lies here only while its frames are read for the bubble's loop, a second or
  so; the loop itself stays in memory.
- `notify/`: a chat's picture for a notification, deleted as soon as the notification has it.
- `cache/voice`, `cache/camera`, `cache/outgoing`, shared-in copies: the user's own recordings,
  photos and picked files on the way out; deleted after sending or when stale.
- `pasted/`: pasted pictures, edited photos and trimmed videos waiting to send; each is deleted
  after the send or when its item is removed, and the whole folder at start.
- Saved to gallery (`Pictures/Unisend`): the user asked for a public copy.

Backup and transfer: `android:allowBackup="false"`, and `res/xml/data_extraction_rules.xml`
excludes every file from cloud backup, device-to-device transfer (which Android 12+ runs despite
`allowBackup="false"` on some phones) and transfer to iOS (Android 16 QPR2+).

## Start, sign-in, sign-out

- Start: no `store.key` means plain (desktop, legacy, or signed out). Otherwise the key is
  unwrapped once and opens everything.
- Every sign-in on Android removes what an earlier one left and makes a new key before the store
  is created. A Keystore failure then fails the sign-in; nothing falls back to plain. A sign-in is
  refused while a session exists that may still open (only signing out deletes it); it replaces
  the files only when there is no session, or this start found the key lost or the session
  unreadable.
- Sign-out, and the server signing the device out (`M_UNKNOWN_TOKEN`), delete the store,
  `store.key`, the sealed and plain session, chat list, drafts, search index, downloads,
  notification pictures and any `*.tmp` left by a cut-short write. Sign-out first waits for the
  sync and recovery tasks to stop, and a download that finishes after it writes nothing. The
  Keystore alias stays and wraps the next key.

## Failure handling

- Permanent key loss signs out; anything else is retried; no error path deletes anything. Only an
  explicit sign-out, the server's `M_UNKNOWN_TOKEN`, or the next sign-in after a loss deletes.
- Lost for good: `DataKey` throws `DataKey$Lost` for no key under the alias (`getKey` returns
  null, which keystore2 does for `KEY_NOT_FOUND`), a key invalidated at `Cipher.init`, a blob
  shorter than its IV, and `AEADBadTagException` on unwrap (keystore2 throws it only for
  `KM_ERROR_VERIFICATION_FAILED`). Also a key file that unwraps to the wrong length, a session
  that does not authenticate with the key, the store refusing the key
  (`OpenStoreError::InitCipher`), or a `store.key` on a platform without a key store. The device
  shows the sign-in screen with a message saying so; the files stay until the next sign-in.
- Unreadable session: `session.enc` cut short, in a newer format (a downgrade), or not valid
  JSON. Signed out the same way, with the files kept.
- Transient: every other exception or I/O error, including keystore2's
  `UnrecoverableKeyException("Failed to obtain information about key")`, which wraps any
  `getKeyEntry` failure but `KEY_NOT_FOUND` (system error, busy backend, a restarted daemon), and
  a store that fails to open for a reason other than its key. The app says it could not open the
  data, asks again after 2 s, doubling up to 60 s, and deletes nothing.
- Drafts that fail to read for a reason that may pass are not written over: a change waits and
  the next save reads them again first.
- A search index that does not open is a lost cache: it starts again empty.
- Logs name the file and the exception class, never key bytes, tokens or message text.

## Migration

An Android install with a plain store and no `store.key` keeps working unencrypted. Settings,
Security shows "Data on this device is not encrypted": signing out and in again encrypts it.
Message history comes back after that sign-in only with the recovery key (or another signed-in
device). Nothing is converted or deleted automatically.

## Cost

- One Keystore call per start (unwrap), one per sign-in (wrap).
- One AES-256-GCM pass per sealed read or write (hardware AES on arm64), plus 29 bytes per file;
  no key derivation, the key is random.
- The SDK encrypts each value it stores (XChaCha20-Poly1305) and hashes keys (keyed BLAKE3).
- Search index: 50 000 messages at most (about 16 MB for short chat lines), the first 4 KB of each
  message, the oldest pruned once more than 20 MiB of pages are in use, and the database capped at
  24 MiB. Sealed, it is held in memory (SQLite's memdb), and each save seals one copy in place and
  writes it: at most 48 MiB for a moment, at most every 5 minutes and only after a change. A
  start opens the file in the buffer it was read into and gives SQLite one copy, also 2x. A kill
  loses at most the last 5 minutes of indexing.
- Downloads sweep: one directory listing and a stat per event folder every 5 minutes and per
  download.
