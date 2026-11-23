# Android

Status: the main target, built and tested on a phone. No store listing.

## Build, install, logs

    cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
    cd android && ./gradlew assembleDebug
    adb install -r app/build/outputs/apk/debug/app-debug.apk
    adb logcat -s beeper-gpui

- Needs the Android SDK and NDK (`ANDROID_NDK_HOME`), `cargo-ndk` and the `aarch64-linux-android`
  Rust target. An emulator takes `-t x86_64` and the `x86_64-linux-android` target.
- A desktop build skips every `cfg(target_os = "android")` block, so only the `cargo ndk` build
  proves Android code compiles, and only a phone proves it works.
- Test installs are the developer build: `UNISEND_DEV=1` on both `cargo ndk` and `gradlew` makes
  "Unisend Dev" (`dev.gpui.beeper.dev`), which sits beside the release app with its own data and
  offers no updates. Build them with `--profile fast` in place of `--release`: no LTO and
  incremental, so a change recompiles only the app's crate. Releases keep `--release`.
- `KLIPY_API_KEY` and `UNISEND_UPDATE_URL` are read at build time: see
  [development.md](../development.md). A signed release: `scripts/release.sh`, whose header lists
  its environment.

## Where Rust and Java meet

Everything the app does is Rust: the UI, Matrix, encryption, storage. Java is there only where
Android accepts nothing else: classes to extend or register (the Activity, the keyboard's
`InputConnection`, a foreground `Service`, the receiver of notification buttons, the share sheet),
and small static helpers through which Rust reaches the rest of the Android API.

- Start: Android loads the cdylib `libbeeper_gpui.so` and calls `android_main` (`src/lib.rs`).
  `src/main.rs` is unused here.
- Rust calls Java: a module under `#[cfg(target_os = "android")]` names its class in `const CLASS`
  and calls its static methods through `gpui_mobile::android::jni` (`with_env`, `find_app_class`).
  Copy `android_lang` in `src/lib.rs`.
- Java calls Rust: a `native` method is the Rust function
  `Java_dev_gpui_<package>_<Class>_<method>`. List them with `rg 'fn Java_' src`.
- Both directions bind by name when the call runs. A renamed class, method or changed signature
  still compiles and fails on the phone: change the Java and the Rust string in one edit, and
  `rg` the old name in `src/` and `android/` to zero hits.
- `android/app/src/main/java/dev/gpui/mobile` comes from gpui-mobile (Apache-2.0); the README
  there names the files changed here. `android/app/src/main/java/dev/gpui/beeper` holds this
  app's Android ends: notifications and the background connection, sharing, files and the
  gallery, camera, voice messages, video playback, location, contacts, app lock, the
  Keystore-wrapped data key, network sign-in, updates and the per-app language.

## Where things live

| Need | Place |
|---|---|
| Keyboard: composing text, autocorrect, spell check | `src/ime.rs`, `GpuiInputActivity.java` |
| Space under system bars and the keyboard | `crate::insets()` in `src/lib.rs` |
| Notifications, reply and mark as read | `Notify.java`, `NotifyReceiver.java`; in `src/lib.rs`, `android_files` calls out and `from_java` is called back |
| Notification sounds (channels `<kind>-<tone>` such as `messages-dew`, since a channel's sound is fixed when it is made; the choice in the `notify-sounds` preferences) | `res/raw/tone_*.ogg`, made by `scripts/sounds.py`; `Notify.channels`, `src/tones.rs` |
| Video trim, and the MP4 made of a video a network refuses | `VideoTrim.java`, `VideoConvert.java` (MediaCodec); called from `src/ui/caption/trim.rs` and `src/backend/mp4.rs` |
| Background connection (no push service) | `SyncService.java` |
| Share sheet, Direct Share | `src/share.rs`, `Share.java`, `src/ui/sharein.rs` |
| Text on Android's own screens | `res/values/strings.xml`, `res/values-pl/strings.xml` ([i18n.md](../i18n.md)) |
| Data at rest, the Keystore key | [data-encryption.md](../data-encryption.md) |
| App lock, screen security | [applock.md](../applock.md) |
| Permissions and components | `android/app/src/main/AndroidManifest.xml` |
