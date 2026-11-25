# Windows

Status: builds on CI (the `build` workflow; its `.exe` is in the releases), never run. Everything
below is read from the source, with no Windows machine behind it.

## Check and build

    cargo check --target x86_64-pc-windows-gnu                             # from any host with mingw-w64
    cargo build --release --locked --bin beeper-gpui --target x86_64-pc-windows-msvc    # on Windows

- The check is all another host can do: a real build needs a Windows host, where GPUI compiles its
  shaders with `fxc.exe` from the Windows SDK.
- Name the target on Windows. `.cargo/config.toml` links the C runtime statically for
  `x86_64-pc-windows-msvc` only, and without `--target` the flag also reaches build scripts.
- A change to Windows code is reported as "compiles, not run" until someone runs it.

## What differs from the other desktops

| Thing | Windows | Place |
|---|---|---|
| Closing the window | quits: there is no tray icon | `CAN_KEEP_RUNNING` in `src/desktop.rs` |
| The count of waiting chats | a red circle with the number over the taskbar button (`ITaskbarList3::SetOverlayIcon`, drawn with GDI); compiles, not run | `src/desktop/taskbar.rs` |
| A second launch | ends without raising the first (no socket) | the `shortcut:` note in `src/instance.rs` |
| Notifications | WinRT toasts with a reply box, under an AppUserModelID the app writes to `HKCU`; buttons answer only while the app runs | `src/notify/windows.rs` |
| Console | none in release builds, so logs go nowhere; debug builds keep it | `src/main.rs` |
| Data | `%LOCALAPPDATA%\beeper-gpui`, plain on disk | `run_desktop` in `src/lib.rs` |
| Icon | `packaging/windows/icon.ico` exists and nothing embeds it in the `.exe` yet | `packaging/icon/render.sh` |

Unsigned builds meet SmartScreen's warning.
