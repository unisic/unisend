#!/bin/sh
# Packs Unisend into an AppImage: the binary, the sign-in browser when it was built, the launcher and
# the icons. Libraries stay the system's (GTK, WebKitGTK, GStreamer, xkbcommon), as with install.sh;
# the AppImage carries only what Unisend itself is.
# Usage: packaging/linux/appimage.sh <dir with beeper-gpui> <out.AppImage>
# Needs appimagetool on PATH or at $APPIMAGETOOL; it fetches its runtime from GitHub.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:?usage: appimage.sh <dir with beeper-gpui> <out.AppImage>}
out=${2:?usage: appimage.sh <dir with beeper-gpui> <out.AppImage>}
tool=${APPIMAGETOOL:-appimagetool}
[ -x "$bin/beeper-gpui" ] || { echo "no binary at $bin/beeper-gpui" >&2; exit 1; }

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
app=$dir/Unisend.AppDir
install -Dm755 "$bin/beeper-gpui" "$app/usr/bin/beeper-gpui"
# Found beside the binary at run time, as install.sh puts it.
if [ -x "$bin/beeper-gpui-weblogin" ]; then install -Dm755 "$bin/beeper-gpui-weblogin" "$app/usr/bin/beeper-gpui-weblogin"; fi
mkdir -p "$app/usr/share"
cp -r "$here/icons" "$app/usr/share/icons"
install -Dm644 "$here/dev.gpui.beeper.desktop" "$app/usr/share/applications/dev.gpui.beeper.desktop"
ln -s usr/share/applications/dev.gpui.beeper.desktop "$app/dev.gpui.beeper.desktop"
ln -s usr/share/icons/hicolor/256x256/apps/dev.gpui.beeper.png "$app/dev.gpui.beeper.png"
ln -s usr/bin/beeper-gpui "$app/AppRun"
# Unquoted: APPIMAGETOOL may carry a flag, as "appimagetool --appimage-extract-and-run" where FUSE is missing.
# shellcheck disable=SC2086
ARCH=x86_64 $tool --no-appstream "$app" "$out"
