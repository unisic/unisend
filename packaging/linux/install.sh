#!/bin/sh
# Installs Unisend for the current user: the binary to ~/.local/bin, the launcher and the icons to
# ~/.local/share. Running it again updates them, a running Unisend included (it keeps its old copy
# until it quits). Usage: packaging/linux/install.sh [path/to/beeper-gpui]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-$here/../../target/release/beeper-gpui}
[ -x "$bin" ] || { echo "no binary at $bin: build it with 'cargo build --release' or pass its path" >&2; exit 1; }
data=${XDG_DATA_HOME:-$HOME/.local/share}
dest=$HOME/.local/bin/beeper-gpui

# A new file renamed over the old one: overwriting the binary of a running copy in place fails.
install -Dm755 "$bin" "$dest.new"
mv -f "$dest.new" "$dest"
# The sign-in browser goes beside it when it was built; without it the system's browser opens.
helper=$(dirname "$bin")/beeper-gpui-weblogin
if [ -x "$helper" ]; then
    install -Dm755 "$helper" "$dest-weblogin.new"
    mv -f "$dest-weblogin.new" "$dest-weblogin"
fi
mkdir -p "$data/applications"
# The launcher names the binary by its full path: ~/.local/bin is not on every session's PATH.
sed "s|^Exec=.*|Exec=$dest|" "$here/dev.gpui.beeper.desktop" > "$data/applications/dev.gpui.beeper.desktop"
(cd "$here/icons" && find hicolor -type f) | while read -r f; do
    install -Dm644 "$here/icons/$f" "$data/icons/$f"
done
if command -v update-desktop-database >/dev/null; then update-desktop-database -q "$data/applications" || true; fi
if command -v gtk-update-icon-cache >/dev/null; then gtk-update-icon-cache -qtf "$data/icons/hicolor" || true; fi
# A running Plasma keeps the icon it loaded first: this is the reload a new icon theme sends.
if command -v dbus-send >/dev/null; then dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 || true; fi
echo "installed $dest and the launcher in $data/applications"
