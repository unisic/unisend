# beeper-gpui

Beeper chat client in Rust: [GPUI](https://www.gpui.rs) via gpui-kit for the UI,
gpui-mobile for Android, matrix-sdk against `matrix.beeper.com`.

Login uses Beeper's email-code flow (`api.beeper.com/user/login`, same as
`bbctl`), then Matrix JWT login. Encrypted history is unlocked with the Beeper
recovery code.

What works and what Beeper has that this does not yet: [docs/parity.md](docs/parity.md).

## Desktop (Linux)

    cargo run --release

Fedora without `libxkbcommon-x11-devel`: the linker needs the unversioned
`libxkbcommon-x11.so`. Without root:

    mkdir -p target/linkshim
    ln -sf /usr/lib64/libxkbcommon-x11.so.0 target/linkshim/libxkbcommon-x11.so
    RUSTFLAGS="-L $PWD/target/linkshim" cargo run --release

Windows and macOS targets come from GPUI and are not built or tested yet.

## Android

Needs the NDK, `cargo-ndk` and the `aarch64-linux-android` / `x86_64-linux-android`
Rust targets.

    cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
    cd android && ./gradlew assembleDebug
    adb install -r app/build/outputs/apk/debug/app-debug.apk

The Java classes in `android/app/src/main/java/dev/gpui/mobile` come from
gpui-mobile (Apache-2.0, see the README there).

## Tests

`src/backend/tests.rs` runs login, sync, E2EE send/decrypt and key recovery
(wrong key, malformed key, history and previews decrypted from the key backup)
against a local Synapse with JWT login enabled:

    MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib login_sync -- --ignored

`gui_fixture` seeds the same server for manual GUI runs: encrypted chats with a
key backup, a room avatar, a bridged room. It reads `avatar.png` from
`GUI_FIXTURE_DIR` and writes `session.json` (a new device) and `recovery-key`
there:

    GUI_FIXTURE_DIR=/tmp/fixture MATRIX_TEST_HS=... MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib gui_fixture -- --ignored

Debug desktop builds read `BEEPER_GPUI_HOMESERVER` to point at such a server
and `BEEPER_GPUI_DARK` to force the dark palette; `BEEPER_GPUI_DATA_DIR` picks
the data directory (with a pre-seeded `session.json`).
