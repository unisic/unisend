#!/bin/sh
# Installs Unisend for the current user: the binary to ~/.local/bin, the launcher and the icons to
# ~/.local/share. Running it again updates them, a running Unisend included (it keeps its old copy
# until it quits). Usage: packaging/linux/install.sh [path/to/beeper-gpui]
# UNISEND_DEV=1 installs a build made with UNISEND_DEV=1 as "Unisend Dev", beside Unisend: its own
# binary, launcher and gray icon.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-$here/../../target/release/beeper-gpui}
[ -x "$bin" ] || { echo "no binary at $bin: build it with 'cargo build --release' or pass its path" >&2; exit 1; }
data=${XDG_DATA_HOME:-$HOME/.local/share}
if [ "${UNISEND_DEV:-}" = 1 ]; then id=dev.gpui.beeper.dev name="Unisend Dev" icons=icons-dev dest=$HOME/.local/bin/beeper-gpui-dev
else id=dev.gpui.beeper name=Unisend icons=icons dest=$HOME/.local/bin/beeper-gpui; fi

# A new file renamed over the old one: overwriting the binary of a running copy in place fails.
install -Dm755 "$bin" "$dest.new"
mv -f "$dest.new" "$dest"
# The sign-in browser goes beside it when it was built; without it the system's browser opens.
helper=$(dirname "$bin")/beeper-gpui-weblogin
if [ -x "$helper" ]; then
    # One name for both builds: the app looks for `beeper-gpui-weblogin` beside itself.
    install -Dm755 "$helper" "$HOME/.local/bin/beeper-gpui-weblogin.new"
    mv -f "$HOME/.local/bin/beeper-gpui-weblogin.new" "$HOME/.local/bin/beeper-gpui-weblogin"
fi
mkdir -p "$data/applications"
# The launcher names the binary by its full path: ~/.local/bin is not on every session's PATH.
sed -e "s|^Exec=.*|Exec=$dest|" -e "s|^Name=.*|Name=$name|" -e "s|dev\.gpui\.beeper\$|$id|" \
    "$here/dev.gpui.beeper.desktop" > "$data/applications/$id.desktop"
(cd "$here/$icons" && find hicolor -type f) | while read -r f; do
    install -Dm644 "$here/$icons/$f" "$data/icons/$f"
done
if command -v update-desktop-database >/dev/null; then update-desktop-database -q "$data/applications" || true; fi
if command -v gtk-update-icon-cache >/dev/null; then gtk-update-icon-cache -qtf "$data/icons/hicolor" || true; fi
# A running Plasma keeps the icon it loaded first: this is the reload a new icon theme sends.
if command -v dbus-send >/dev/null; then dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 || true; fi
echo "installed $dest and the launcher in $data/applications"
