# Linux

Status: built and tested. The notes and patches here come from Fedora with KDE Plasma on Wayland;
X11 is compiled in too.

## Build, run, install

    cargo run                     # debug, against the real account in ~/.local/share/beeper-gpui
    cargo build --release
    packaging/linux/install.sh    # binary to ~/.local/bin, launcher and icons to ~/.local/share
    packaging/linux/appimage.sh target/release Unisend-x86_64.AppImage   # or one AppImage, system libraries outside it

A test install takes `cargo build --profile fast` and `packaging/linux/install.sh
target/fast/beeper-gpui`: as optimized, but without LTO and incremental, so after a change only
the app's own crate compiles again, in parallel (`Cargo.toml` says how). Shipped builds use
`--release`.

Needs a stable Rust toolchain and the development packages for xcb, xkbcommon (with x11),
fontconfig, freetype, wayland and GStreamer (voice messages, below). A second copy on the same
data directory hands over to the first and ends (`src/instance.rs`): run a test copy with
`BEEPER_GPUI_DATA_DIR` set ([development.md](../development.md)).

Fedora without `libxkbcommon-x11-devel`: the linker needs the unversioned `libxkbcommon-x11.so`.
Without root:

    mkdir -p target/linkshim
    ln -sf /usr/lib64/libxkbcommon-x11.so.0 target/linkshim/libxkbcommon-x11.so
    RUSTFLAGS="-L $PWD/target/linkshim" cargo run --release

## Voice messages (GStreamer)

Voice messages play and record inside the chat through the system's GStreamer (`gstreamer`
crate, src/ui/voice/linux.rs). Building needs its development package (`gstreamer1-devel` on
Fedora, `libgstreamer1.0-dev` on Debian and Ubuntu). Without it, as on a machine with no root, point
the build at the runtime library instead of its pkg-config file:

    ln -sf /usr/lib64/libgstreamer-1.0.so.0 target/linkshim/libgstreamer-1.0.so
    export SYSTEM_DEPS_GSTREAMER_1_0_NO_PKG_CONFIG=1 SYSTEM_DEPS_GSTREAMER_1_0_LIB=gstreamer-1.0
    RUSTFLAGS="-L $PWD/target/linkshim" cargo build

The binary then needs `libgstreamer-1.0.so.0` to start, from the distribution's `gstreamer1`
(Fedora) or `libgstreamer1.0-0` (Debian, Ubuntu) package; without it the app does not start at all.
Playing and recording also need the base and good plugins (`playbin`, `scaletempo`, `opusenc`,
`oggmux`, `autoaudiosrc`, `autoaudiosink`); when one is missing, or a file has no decoder, audio
opens in the system's player and there is no microphone button, as on Windows and macOS.

What it records is what Android records: Ogg Opus, mono, 48 kHz, 24 kbit/s, sent as an MSC3245
voice message. Recordings wait in
`~/.cache/beeper-gpui/voice` (mode 700) and are deleted once sent, thrown away, on leaving the chat,
on closing the window and on quitting; one left by a crash goes when the next recording starts, if
a day old. Nothing leaves the device but the sent message. The sound server (PipeWire or
PulseAudio) picks the microphone and the speakers.

Cost, measured with `gst-launch-1.0` on the same pipelines: GStreamer loads at startup on the
voice thread (about 8 MB). One player and one recorder at a time: a player about 8 MB and five
threads (streaming and the sound server's), a recorder about 3 MB and three, all stopped with it;
GLib's thread pool keeps an idle one for a while. Decoding or encoding a voice note takes under 1%
of one core.

## Videos (GStreamer)

A video plays in the app's own player over the window (src/ui/video/linux.rs): `playbin` decodes,
`videoconvert`, `videoflip` (turns an upright phone video by its orientation tag), `videoscale` and
`appsink` hand each frame over as BGRA, and the newest is drawn as a picture. Play and pause (click,
Space or K), seek on the bar, Left and Right skip 5 s, M or the speaker mutes, Esc or the X closes. A
bridged GIF loops without sound. A video GStreamer cannot decode (a codec its plugins lack) closes
the player and opens in the system's app, as before.

Cost while open: one decoded frame waiting (8 MB at 1080p) and one on the screen as a texture, each
replaced by the next frame, so 30 frames a second copy and upload 30 of them; the decoder's own
threads and buffers on top. A timer looks for a new frame every 8 ms, which only reads a lock. All
of it stops and goes with the player.

## Signing in to a network in a browser (WebKitGTK)

Networks whose bridge signs in with cookies (Messenger, Instagram and others) open their login page
in a window of the app's own, as Android does: `web_login` in `src/lib.rs` starts
`beeper-gpui-weblogin` (`weblogin/`, a program of its own) from the app's directory with the
request on stdin, and that process shows a WebKitGTK 4.1 view in a GTK 3 window, so GTK's main loop
never meets GPUI's. It closes once the address matches the bridge's pattern and every required cookie, local storage key or script result has a
value, and prints them as one JSON line; closed early, it prints nothing and the step waits for a
paste. The profile is ephemeral: cookies, storage and cache live in that process's memory and end
with it. Location, camera, microphone and notification requests are refused; windows a page opens
load in the same view. When the window cannot open (no helper, no WebKitGTK, no display), the page
opens in the system's browser and the user pastes, as on Windows and macOS.

Building needs WebKitGTK 4.1, 2.40 or later, and GTK 3 development files (`webkit2gtk4.1-devel`
and `gtk3-devel` on Fedora, `libwebkit2gtk-4.1-dev` and `libgtk-3-dev` on Debian and Ubuntu).
Only the helper links them and needs `libwebkit2gtk-4.1.so.0`, from `webkit2gtk4.1` (Fedora) or
`libwebkit2gtk-4.1-0` (Debian, Ubuntu); the app starts without it. `packaging/linux/install.sh`
installs the helper beside the app when it was built.

Cost: the app loads no WebKitGTK or GTK. The browser runs only while the window is open, as three
processes (the helper, WebKit's web and network processes): about 300 MB PSS together on a test
page in the GUI sandbox, debug build with software rendering. While open it reads local storage and the script's result every 500 ms, and the
cookie jar after each probe once the address matches. Nothing is written to disk.

## Where things live

| Need | Place |
|---|---|
| Title bar and window buttons drawn by the app | `src/ui/frame.rs`; a compositor that insists on its own title bar wins |
| Tray icon (an accent dot while a chat waits), running on with the window closed | `src/desktop/tray.rs` (StatusNotifierItem over zbus), `src/desktop.rs` |
| Notifications with reply, mark as read, mute | `src/notify/linux.rs` (`org.freedesktop.Notifications`; the reply box is KDE's) |
| One copy per data directory, raise on a second launch | `src/instance.rs` |
| A video a network refuses, made into an MP4 when the user agrees: the system's `ffmpeg` on `PATH`, optional (without it the offer is not made) | `src/backend/mp4.rs` |
| Signing in to a network in a browser window | `src/weblogin.rs`, started by `web_login` in `src/lib.rs` |
| Launcher and icons | `packaging/linux/`; the window's `app_id` in `src/lib.rs` must stay the `.desktop` file's name, or the shell shows no icon |
| Fixes to GPUI's Wayland layer (raise from a notification, pasting files) | `vendor/gpui-pre-linux/PATCHES.md` |

The store and files are plain on disk: no key store is wired on desktop
([data-encryption.md](../data-encryption.md)). App lock is the app PIN only
([applock.md](../applock.md)).
