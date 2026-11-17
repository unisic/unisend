# Linux

Status: built and tested. The notes and patches here come from Fedora with KDE Plasma on Wayland;
X11 is compiled in too.

## Build, run, install

    cargo run                     # debug, against the real account in ~/.local/share/beeper-gpui
    cargo build --release
    packaging/linux/install.sh    # binary to ~/.local/bin, launcher and icons to ~/.local/share

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
