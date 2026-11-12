# beeper-gpui

Beeper chat client in Rust: [GPUI](https://www.gpui.rs) via gpui-kit for the UI,
gpui-mobile for Android, matrix-sdk against `matrix.beeper.com`.

Login uses Beeper's email-code flow (`api.beeper.com/user/login`, same as
`bbctl`), then Matrix JWT login. Encrypted history is unlocked with the Beeper
recovery code.

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
against a local Synapse with JWT login enabled:

    MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib -- --ignored

Debug desktop builds read `BEEPER_GPUI_HOMESERVER` to point at such a server;
`BEEPER_GPUI_DATA_DIR` picks the data directory (with a pre-seeded `session.json`).
