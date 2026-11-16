# Linux

Status: built and tested. The notes and patches here come from Fedora with KDE Plasma on Wayland;
X11 is compiled in too.

## Build, run, install

    cargo run                     # debug, against the real account in ~/.local/share/beeper-gpui
    cargo build --release
    packaging/linux/install.sh    # binary to ~/.local/bin, launcher and icons to ~/.local/share

Needs a stable Rust toolchain and the development packages for xcb, xkbcommon (with x11),
fontconfig, freetype and wayland. A second copy on the same data directory hands over to the first
and ends (`src/instance.rs`): run a test copy with `BEEPER_GPUI_DATA_DIR` set
([development.md](../development.md)).

Fedora without `libxkbcommon-x11-devel`: the linker needs the unversioned `libxkbcommon-x11.so`.
Without root:

    mkdir -p target/linkshim
    ln -sf /usr/lib64/libxkbcommon-x11.so.0 target/linkshim/libxkbcommon-x11.so
    RUSTFLAGS="-L $PWD/target/linkshim" cargo run --release

## Where things live

| Need | Place |
|---|---|
| Title bar and window buttons drawn by the app | `src/ui/frame.rs`; a compositor that insists on its own title bar wins |
| Tray icon, running on with the window closed | `src/desktop/tray.rs` (StatusNotifierItem over zbus), `src/desktop.rs` |
| Notifications with reply, mark as read, mute | `src/notify/linux.rs` (`org.freedesktop.Notifications`; the reply box is KDE's) |
| One copy per data directory, raise on a second launch | `src/instance.rs` |
| Launcher and icons | `packaging/linux/`; the window's `app_id` in `src/lib.rs` must stay the `.desktop` file's name, or the shell shows no icon |
| Fixes to GPUI's Wayland layer (raise from a notification, pasting files) | `vendor/gpui-pre-linux/PATCHES.md` |

The store and files are plain on disk: no key store is wired on desktop
([data-encryption.md](../data-encryption.md)). App lock is the app PIN only
([applock.md](../applock.md)).
