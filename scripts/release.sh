#!/bin/bash
# Builds a signed release APK and the update.json that announces it, into dist/<version>/.
# Upload both; the app reads update.json at MURMUR_UPDATE_URL and fetches the APK it names.
#
# Usage: scripts/release.sh <versionName> [notes]
#
# Environment (nothing of the key is ever kept in this repository):
#   MURMUR_KEYSTORE, MURMUR_KEYSTORE_PASSWORD, MURMUR_KEY_ALIAS, MURMUR_KEY_PASSWORD
#       The release key. Keep it and its passwords outside the repo, and keep a backup: every
#       update must be signed with the same key, or Android refuses to install it.
#   MURMUR_UPDATE_URL   https URL the app checks, baked into this build, e.g.
#       https://github.com/<owner>/<repo>/releases/latest/download/update.json
#   MURMUR_APK_BASE     https folder the APK will be served from, e.g.
#       https://github.com/<owner>/<repo>/releases/download/v<versionName>
#   ANDROID_NDK_HOME, cargo-ndk, and optionally KLIPY_API_KEY, as for any Android build.
#
# The version code is the commit count of HEAD, so every release must come from a newer commit.
set -euo pipefail

name=${1:?usage: scripts/release.sh <versionName> [notes]}
notes=${2:-}
for var in MURMUR_KEYSTORE MURMUR_KEYSTORE_PASSWORD MURMUR_KEY_ALIAS MURMUR_KEY_PASSWORD MURMUR_UPDATE_URL MURMUR_APK_BASE; do
    [ -n "${!var:-}" ] || { echo "$var is not set" >&2; exit 1; }
done
[ -f "$MURMUR_KEYSTORE" ] || { echo "no keystore at MURMUR_KEYSTORE" >&2; exit 1; }
case "$MURMUR_UPDATE_URL$MURMUR_APK_BASE" in
    https://*https://*) ;;
    *) echo "MURMUR_UPDATE_URL and MURMUR_APK_BASE must be https" >&2; exit 1 ;;
esac

cd "$(git rev-parse --show-toplevel)"
# A build from uncommitted changes would share its version code with the next commit's.
git diff --quiet HEAD || { echo "commit or stash changes first" >&2; exit 1; }
code=$(git rev-list --count HEAD)
apk_name="murmur-$name.apk"
out=dist/$name
mkdir -p "$out"

export MURMUR_UPDATE_URL MURMUR_VERSION_CODE=$code MURMUR_VERSION_NAME=$name
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release --lib
(cd android && ./gradlew --no-daemon -q assembleRelease)

apk=android/app/build/outputs/apk/release/app-release.apk
[ -f "$apk" ] || { echo "no signed APK at $apk" >&2; exit 1; }
cp "$apk" "$out/$apk_name"
sum=$(sha256sum "$out/$apk_name" | cut -d' ' -f1)
jq -n --argjson code "$code" --arg name "$name" --arg url "${MURMUR_APK_BASE%/}/$apk_name" --arg sha "$sum" --arg notes "$notes" \
    '{versionCode: $code, versionName: $name, url: $url, sha256: $sha, notes: $notes}' > "$out/update.json"

echo "versionCode $code, sha256 $sum"
echo "upload $out/$apk_name to ${MURMUR_APK_BASE%/}/ and $out/update.json to $MURMUR_UPDATE_URL"
