# Development

How to build, test and package Unisend beyond the short steps in the [README](../README.md).
The crate and binary are named `beeper-gpui`, the Android app id is `dev.gpui.beeper`.

## Fedora linker note

Fedora without `libxkbcommon-x11-devel`: the linker needs the unversioned
`libxkbcommon-x11.so`. Without root:

    mkdir -p target/linkshim
    ln -sf /usr/lib64/libxkbcommon-x11.so.0 target/linkshim/libxkbcommon-x11.so
    RUSTFLAGS="-L $PWD/target/linkshim" cargo run --release

## Windows and macOS

`.github/workflows/build.yml` builds both on GitHub's runners (also Linux and the Android APK); it
has never run, because the repository has never been pushed. A Windows build cannot be made on
Linux: GPUI compiles its shaders with `fxc.exe`, on a Windows host only. iOS has no port yet: see
[parity.md](parity.md).

## GIF picker (KLIPY)

The GIF picker appears only when the build has a KLIPY key ("Create API Key" on docs.klipy.com):
`export KLIPY_API_KEY=<your key>` before `cargo ndk` (or `cargo build` on desktop). Picker requests
go straight from the app to KLIPY: KLIPY sees the device's IP address, the search words, the GIF
sent and a random ID made once per app start.

## Updates outside an app store

A build with `UNISEND_UPDATE_URL` (https) checks that address for an `update.json` (`versionCode`,
`versionName`, `url`, `sha256`, `notes`) once a day while "Check for updates automatically" is on,
and on "Check now". A newer release is downloaded only after the user taps Update, checked against
its SHA-256 and handed to Android's installer, which refuses one signed with another key. The
request is a plain GET without a user agent, cookie or identifier: the host (and, for GitHub, the
download servers it redirects to) sees the phone's IP address, the time and the URL. Without the
variable, Settings has no Updates section. `scripts/release.sh` builds a release signed with a key
from the environment and writes `update.json`; a GitHub release can serve it at the stable
`https://github.com/<owner>/<repo>/releases/latest/download/update.json`, any static host works.

## Java in a Rust app

Everything the app does is Rust: the UI, Matrix, encryption, storage. Java is there only where
Android accepts nothing else: some platform parts exist only as Java classes to extend or
register (the Activity, the keyboard's `InputConnection`, a foreground `Service`, the receiver of
notification buttons, the share sheet), and Rust reaches the rest of the Android API over JNI
through small Java helpers. `android/app/src/main/java/dev/gpui/mobile` comes from gpui-mobile
(Apache-2.0, see the README there, which names the files changed here);
`android/app/src/main/java/dev/gpui/beeper` holds this app's Android ends: notifications and the
background connection, sharing, files and the gallery, camera, voice messages, video playback,
location, contacts, app lock, the Keystore-wrapped data key, network sign-in, updates and the
per-app language.

## Tests

`src/backend/tests.rs` runs login, sync, E2EE send/decrypt and key recovery
(wrong key, malformed key, history and previews decrypted from the key backup)
against a local Synapse with JWT login enabled:

    MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib login_sync -- --ignored

`password_login_restore_sign_out` signs in with a password (wrong password,
http refused, unknown server, restore after restart, sign out); it creates its
user through Synapse's shared-secret registration:

    MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_REGISTRATION_SECRET=... \
      cargo test --lib password_login -- --ignored

`gui_fixture` seeds the same server for manual GUI runs: encrypted chats with a
key backup, a room avatar, a bridged room. It reads `avatar.png` from
`GUI_FIXTURE_DIR` and writes `session.json` (a new device) and `recovery-key`
there:

    GUI_FIXTURE_DIR=/tmp/fixture MATRIX_TEST_HS=... MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib gui_fixture -- --ignored

## Environment variables

Debug desktop builds read `BEEPER_GPUI_HOMESERVER` to point at such a server,
and `BEEPER_GPUI_API` and `BEEPER_GPUI_BRIDGES` to point the account API and the
bridge login API at stand-ins; `BEEPER_GPUI_DATA_DIR` picks the data directory
(with a pre-seeded `session.json`, and `settings.json` for the theme).
