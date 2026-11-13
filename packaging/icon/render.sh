#!/bin/sh
# Regenerates every raster icon. The master is murmuration.py, which writes the SVGs rendered here.
# Needs ImageMagick 7 built with librsvg, and python3 with numpy.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root="$here/../.."
res="$root/android/app/src/main/res"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
svg="$tmp/svg"
python3 "$here/murmuration.py" "$svg"

# Renders an SVG at high density, then downsamples, so small sizes stay sharp.
png() { magick -density 1200 -background none "$1" -resize "$2x$2" -strip "PNG32:$3"; }
# The desktop tile in its optical size: fewer, larger birds up to 192 px, the fine flock above.
tile() { if [ "$1" -le 192 ]; then echo "$svg/icon-small.svg"; else echo "$svg/icon.svg"; fi; }

for s in 48 128 256 512; do
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

# ImageMagick cannot write ICNS; the container is a header plus PNG chunks keyed by OSType.
for s in 16 32 64 128 256 512 1024; do png "$(tile "$s")" "$s" "$tmp/icns-$s.png"; done
python3 - "$tmp" "$root/packaging/macos/Murmur.icns" <<'PY'
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

# density:legacy icon (48 dp):adaptive layer (108 dp), in px
for d in mdpi:48:108 hdpi:72:162 xhdpi:96:216 xxhdpi:144:324 xxxhdpi:192:432; do
    n=${d%%:*}; a=${d##*:}; s=${d#*:}; s=${s%:*}; h=$((s / 2))
    png "$svg/icon-small.svg" "$s" "$res/mipmap-$n/ic_launcher.png"
    # The circle inscribed in the tile clears its rounded corners, so masking the tile is enough.
    magick -density 1200 -background none "$svg/icon-small.svg" -resize "${s}x${s}" \
        \( -size "${s}x${s}" xc:black -fill white -draw "circle $h,$h $h,0" \) \
        -alpha off -compose CopyOpacity -composite -strip "PNG32:$res/mipmap-$n/ic_launcher_round.png"
    # Thousands of birds are too heavy for a vector drawable: adaptive layers and splash are
    # bitmaps at each density's size of the 108 dp layer.
    mkdir -p "$res/drawable-$n"
    png "$svg/foreground.svg" "$a" "$res/drawable-$n/ic_launcher_foreground.png"
    png "$svg/monochrome.svg" "$a" "$res/drawable-$n/ic_launcher_monochrome.png"
    png "$svg/splash.svg" "$a" "$res/drawable-$n/splash_icon.png"
done
