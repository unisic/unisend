# Data at rest

How Murmur keeps what it stores on the phone. Code: `src/vault.rs`,
`android/app/src/main/java/dev/gpui/beeper/DataKey.java`, `src/backend.rs` (`unlock`, `lost`,
`fresh_store`, `wipe_account`), `src/backend/search.rs`.

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
| `search.enc` | message search index (FTS5), kept in memory, saved every 5 minutes while it changes | sealed |
| `rooms-lang.txt`, `settings.json`, `emoji-recents.json`, `hidden-<account>.txt` (ids of hidden messages), `wallpaper.jpg`, `klipy-id`, app PIN hash, emoji font | no chat content | plain |

A sealed file is `1 | nonce (12, random) | AES-256-GCM ciphertext | tag (16)`, and the plain file
name is authenticated with it, so one file cannot be swapped for another.

What the SDK leaves readable in `store/` (matrix-sdk-sqlite 0.19.1): table and index names, row
counts and sizes, integer columns (timestamps, send queue priority and order, chunk links), the
relation type of events (`rel_type`, such as `m.replace`), send queue transaction ids, and the
names of the crypto store's key-value entries. Room, event and user ids are stored hashed;
event contents, state, members, keys and media are encrypted. Values written with
`set_custom_value` are stored plain; only sliding sync and OAuth use them, and this app uses
neither.

Plain on purpose, short-lived:

- `media/`: attachments downloaded to open in another app. Emptied at start and at sign-out.
- `notify/`: a chat's picture for a notification, deleted as soon as the notification has it.
- `cache/voice`, `cache/camera`, `cache/outgoing`, shared-in copies: the user's own recordings,
  photos and picked files on the way out; deleted after sending or when stale.
- Saved to gallery (`Pictures/Murmur`): the user asked for a public copy.

## Start, sign-in, sign-out

- Start: no `store.key` means plain (desktop, legacy, or signed out). Otherwise the key is
  unwrapped once and opens everything.
- Every sign-in on Android removes what an earlier one left and makes a new key before the store
  is created. A Keystore failure then fails the sign-in; nothing falls back to plain.
- Sign-out deletes the store, `store.key`, the sealed and plain session, chat list, drafts,
  search index, downloads and notification pictures. The Keystore alias stays and wraps the next
  key.

## Failure handling

- Lost for good: `KeyPermanentlyInvalidatedException`, `UnrecoverableKeyException` (including a
  missing alias), `AEADBadTagException` on unwrap, a key file that unwraps to the wrong length, a
  session that does not open with the key, the store refusing the key
  (`OpenStoreError::InitCipher`), or a `store.key` on a platform without a key store. The device is treated as signed out with a message saying so,
  and only then are the undecryptable files removed.
- Transient: any other exception or I/O error. The app says it could not open the data, asks
  again after 2 s, doubling up to 60 s, and deletes nothing.
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
- Search index, sealed: held in memory (up to about 16 MB at its 50 000-message cap); each save
  copies, seals and writes all of it, at most every 5 minutes and only after a change. A kill
  loses at most the last 5 minutes of indexing.
