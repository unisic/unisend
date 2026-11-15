# Murmur

Murmur (crate and binary `beeper-gpui`) is a chat client for Beeper accounts, in
Rust: [GPUI](https://www.gpui.rs) via gpui-kit for the UI, gpui-mobile for
Android, matrix-sdk against `matrix.beeper.com`.

Login uses Beeper's email-code flow (`api.beeper.com/user/login`, same as
`bbctl`), then Matrix JWT login. Any other Matrix homeserver that offers
`m.login.password` works too ("Use another Matrix server"); single sign-on, OIDC
and registration do not yet. On other servers the Networks page says that chat
networks are a Beeper feature. Encrypted history is unlocked with the account's
recovery code.

Emoji: on Linux and Android, Settings > Emoji can use a colour emoji font file of your own (sbix,
CBDT or COLRv0, up to 300 MB; applies after a restart); the app ships no such font, so use one you
are licensed to use on that device. macOS draws with the system's emoji.

What works and what Beeper has that this does not yet: [docs/parity.md](docs/parity.md).

## Desktop (Linux)

    cargo run --release

Fedora without `libxkbcommon-x11-devel`: the linker needs the unversioned
`libxkbcommon-x11.so`. Without root:

    mkdir -p target/linkshim
    ln -sf /usr/lib64/libxkbcommon-x11.so.0 target/linkshim/libxkbcommon-x11.so
    RUSTFLAGS="-L $PWD/target/linkshim" cargo run --release

Windows and macOS: `.github/workflows/build.yml` builds both on GitHub's runners (also Linux and
the Android APK); it has never run, because the repository has never been pushed. A Windows
build cannot be made on Linux: GPUI compiles its shaders with `fxc.exe`, on a Windows host only.
iOS has no port yet: see `docs/parity.md`.

## Android

Needs the NDK, `cargo-ndk` and the `aarch64-linux-android` / `x86_64-linux-android`
Rust targets.

    cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
    cd android && ./gradlew assembleDebug
    adb install -r app/build/outputs/apk/debug/app-debug.apk

The GIF picker appears only when the build has a KLIPY key ("Create API Key" on docs.klipy.com):
`export KLIPY_API_KEY=$(cat ~/.config/murmur/klipy.key)` before `cargo ndk`.

The Java classes in `android/app/src/main/java/dev/gpui/mobile` come from
gpui-mobile (Apache-2.0, see the README there).

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

Debug desktop builds read `BEEPER_GPUI_HOMESERVER` to point at such a server,
and `BEEPER_GPUI_API` and `BEEPER_GPUI_BRIDGES` to point the account API and the
bridge login API at stand-ins; `BEEPER_GPUI_DATA_DIR` picks the data directory
(with a pre-seeded `session.json`, and `settings.json` for the theme).
