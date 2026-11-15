#!/bin/sh
# Usage: bundle.sh <binary> <out-dir>
# Assembles Unisend.app around a built binary and signs it ad hoc. Runs on macOS only (codesign).
set -eu
bin=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../Cargo.toml" | head -n 1)
app="$out/Unisend.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp "$bin" "$app/Contents/MacOS/beeper-gpui"
cp "$here/Unisend.icns" "$app/Contents/Resources/Unisend.icns"
chmod 755 "$app/Contents/MacOS/beeper-gpui"
# Ad hoc: no identity, so Gatekeeper still blocks a downloaded copy until the user allows it.
codesign --force --deep --sign - "$app"
codesign --verify --strict "$app"
echo "$app"
