# macOS

Status: in progress. The universal `.app` (arm64 and x86_64, both built for macOS 11.0) builds
and runs on Apple silicon (macOS 27; the x86_64 half under Rosetta), and the unit tests pass
there. CI's `.app` (the `build` workflow) is in the releases; it has not been opened on a Mac yet.

## Build, run, bundle

    cargo run                                                       # debug, from the terminal
    cargo build --release
    sh packaging/macos/bundle.sh target/release/beeper-gpui target/macos    # Unisend.app, signed ad hoc

- Needs Xcode's command line tools; the rest is a plain `cargo build`.
- Universal, as CI makes it: `MACOSX_DEPLOYMENT_TARGET=11.0`, a `--release` build for
  `aarch64-apple-darwin` and `x86_64-apple-darwin`, `lipo -create`, then `bundle.sh`.
- Notifications arrive only from the `.app` bundle (`UNUserNotificationCenter` refuses a bare
  binary): check them from `target/macos/Unisend.app`, never from `cargo run`.
- The bundle is signed ad hoc, so Gatekeeper blocks a downloaded copy until the user allows it.
- `LSMinimumSystemVersion` is 11.0 in `packaging/macos/Info.plist`, matching the binary's
  `minos`; nothing older than the build host has been run, nor a real Intel Mac.
- Data: `~/Library/Application Support/beeper-gpui`, plain on disk
  ([data-encryption.md](../data-encryption.md)). A test copy takes `BEEPER_GPUI_DATA_DIR`
  ([development.md](../development.md)).

## Where things live

| Need| Place |
|---|---|
| Running on with the window closed, a Dock click opens it again | `src/desktop.rs`, `app.on_reopen` in `src/lib.rs` |
| Notifications: buttons, no reply box, picture or sound | `src/notify/macos.rs` |
| The count of waiting chats on the Dock icon (`NSDockTile` badge) | `src/desktop/dock.rs` |
| App icon | Liquid Glass on macOS 26 and later from `packaging/macos/Assets.car`, `Unisend.icns` before; both written by `packaging/icon/render.sh` (needs Xcode 26) from `mac_icon` in `packaging/icon/unisend.py` |

App lock is the app PIN only, asked at start and when a window opens after the last one closed
([applock.md](../applock.md)).
