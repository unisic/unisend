#!/bin/sh
# Regenerates every icon. The master is unisend.py, which writes the SVGs rendered here and the
# Android vector drawables copied here. Needs ImageMagick 7 built with librsvg, python3, and for the
# macOS icon Xcode 26 or later (actool, ictool).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root="$here/../.."
res="$root/android/app/src/main/res"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
svg="$tmp/svg"
python3 "$here/unisend.py" "$svg"

# Renders an SVG at high density, then downsamples, so small sizes stay sharp.
png() { magick -density 1200 -background none "$1" -resize "$2x$2" -strip "PNG32:$3"; }
# The desktop tile in its optical size: the grid-fitted mark with the wider gap up to 48 px.
tile() { if [ "$1" -le 48 ]; then echo "$svg/icon-small.svg"; else echo "$svg/icon.svg"; fi; }

for s in 16 24 32 48 64 128 256 512; do
    mkdir -p "$root/packaging/linux/icons/hicolor/${s}x${s}/apps"
    png "$(tile "$s")" "$s" "$root/packaging/linux/icons/hicolor/${s}x${s}/apps/dev.gpui.beeper.png"
done
mkdir -p "$root/packaging/linux/icons/hicolor/scalable/apps"
cp "$svg/icon.svg" "$root/packaging/linux/icons/hicolor/scalable/apps/dev.gpui.beeper.svg"

for s in 16 24 32 48 64 128 256; do png "$(tile "$s")" "$s" "$tmp/ico-$s.png"; done
# PNG-compressed entries (Windows Vista and later); ImageMagick would store them as raw bitmaps.
python3 - "$tmp" "$root/packaging/windows/icon.ico" <<'PY'
import struct, sys
tmp, out = sys.argv[1], sys.argv[2]
sizes = [16, 24, 32, 48, 64, 128, 256]
data = [open(f"{tmp}/ico-{s}.png", "rb").read() for s in sizes]
head = struct.pack("<HHH", 0, 1, len(sizes))
offset = len(head) + 16 * len(sizes)
for s, d in zip(sizes, data):
    head += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(d), offset)
    offset += len(d)
open(out, "wb").write(head + b"".join(data))
PY

# macOS 26 and later read the Liquid Glass icon from Assets.car, compiled from the Icon Composer document.
# Without Xcode (any non-Mac) the macOS files are left as they are and the rest is still written.
if command -v xcrun >/dev/null; then
mkdir "$tmp/car"
xcrun actool "$svg/Unisend.icon" --compile "$tmp/car" --app-icon Unisend --platform macosx \
    --target-device mac --minimum-deployment-target 11.0 --output-partial-info-plist "$tmp/car/info.plist" >/dev/null
cp "$tmp/car/Assets.car" "$root/packaging/macos/Assets.car"
# Older macOS reads the .icns: the same document rendered by ictool, since actool's .icns stops at 256 px.
ictool="$(xcode-select -p)/../Applications/Icon Composer.app/Contents/Executables/ictool"
for s in 16 32 64 128 256 512 1024; do
    "$ictool" "$svg/Unisend.icon" --export-image --output-file "$tmp/glass-$s.png" --platform macOS \
        --rendition Default --width "$s" --height "$s" --scale 1 >/dev/null
    # ictool writes 16-bit PNGs; 8-bit ones cut the .icns from 3 MB to 0.7 MB.
    magick "$tmp/glass-$s.png" -depth 8 -strip "PNG32:$tmp/icns-$s.png"
done
# ImageMagick cannot write ICNS; the container is a header plus PNG chunks keyed by OSType.
python3 - "$tmp" "$root/packaging/macos/Unisend.icns" <<'PY'
import struct, sys
tmp, out = sys.argv[1], sys.argv[2]
types = [("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128), ("ic08", 256), ("ic09", 512),
         ("ic10", 1024), ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512)]
body = b""
for ostype, size in types:
    data = open(f"{tmp}/icns-{size}.png", "rb").read()
    body += ostype.encode() + struct.pack(">I", 8 + len(data)) + data
open(out, "wb").write(b"icns" + struct.pack(">I", 8 + len(body)) + body)
PY
else
    echo "render.sh: no xcrun, macOS icon not rebuilt" >&2
fi

# The adaptive layers and the splash are vector drawables: sharp at every density and launcher size.
cp "$svg"/*.xml "$res/drawable/"

# Legacy launcher icon (48 dp) per density, in px. With minSdk 26 the adaptive icon always wins;
# these only serve whatever asks for the bitmap directly.
for d in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
    n=${d%%:*}; s=${d#*:}; h=$((s / 2))
    png "$(tile "$s")" "$s" "$res/mipmap-$n/ic_launcher.png"
    # The circle inscribed in the tile clears its rounded corners and the full mark (not the small
    # one, whose tail reaches past it), so masking the tile is enough.
    magick -density 1200 -background none "$svg/icon.svg" -resize "${s}x${s}" \
        \( -size "${s}x${s}" xc:black -fill white -draw "circle $h,$h $h,0" \) \
        -alpha off -compose CopyOpacity -composite -strip "PNG32:$res/mipmap-$n/ic_launcher_round.png"
done
