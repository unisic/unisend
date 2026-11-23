# Development

How to build, test and package Unisend beyond the short steps in the [README](../README.md).
The crate and binary are named `beeper-gpui`, the Android app id is `dev.gpui.beeper`.

## Platforms

Each platform's build steps, its status and where its code lives:
[Android](platforms/android.md), [Linux](platforms/linux.md), [macOS](platforms/macos.md),
[Windows](platforms/windows.md). iOS has no port yet: see [parity.md](parity.md).

`.github/workflows/build.yml` builds all four on GitHub's runners. It starts on a `v*` tag or by
hand, and neither has happened: every step in it is unverified.

## GIF picker (KLIPY)

The GIF picker appears only when the build has a KLIPY key ("Create API Key" on docs.klipy.com):
`export KLIPY_API_KEY=<your key>` before `cargo ndk` (or `cargo build` on desktop). Picker requests
go straight from the app to KLIPY: KLIPY sees the device's IP address, the search words, the GIF
sent and a random ID made once per app start.

## Updates outside an app store

A build with `UNISEND_UPDATE_URL` (https) checks that address for an `update.json` (`versionCode`,
`versionName`, `url`, `sha256`, `notes`) when the user taps Check now, and once a day only if they turn
on "Check for updates automatically" (off by default). A newer release is downloaded only after the user taps Update, checked against
its SHA-256 and handed to Android's installer, which refuses one signed with another key. The
request is a plain GET without a user agent, cookie or identifier: the host (and, for GitHub, the
download servers it redirects to) sees the phone's IP address, the time and the URL. Without the
variable, Settings has no Updates section: a Google Play build leaves it out, since Play updates
the app. `scripts/release.sh` builds a release signed with a key
from the environment and writes `update.json`; a GitHub release can serve it at the stable
`https://github.com/<owner>/<repo>/releases/latest/download/update.json`, any static host works.

## Tests

`cargo test --lib` needs no server: the unit tests beside the code,
the translation check among them ([i18n.md](i18n.md)). It builds for the host, so it covers no
other platform's `cfg` blocks. The tests below are `#[ignore]`d because they need a homeserver.

`src/backend/tests.rs` runs login, sync, E2EE send/decrypt and key recovery
(wrong key, malformed key, history and previews decrypted from the key backup)
against a local Synapse with JWT login enabled:

    MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib login_sync -- --ignored

`password_login_restore_sign_out` signs in with a password (wrong password,
http refused, unknown server, restore after restart, sign out); it creates its
user through Synapse's shared-secret registration:

    MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_REGISTRATION_SECRET=... \
      cargo test --lib password_login -- --ignored

`gui_fixture` seeds the same server for manual GUI runs: encrypted chats with a
key backup, a room avatar, a bridged room. It reads `avatar.png` from
`GUI_FIXTURE_DIR` and writes `session.json` (a new device) and `recovery-key`
there:

    GUI_FIXTURE_DIR=/tmp/fixture MATRIX_TEST_HS=... MATRIX_TEST_JWT_SECRET=... \
      cargo test --lib gui_fixture -- --ignored

## Environment variables

Debug desktop builds read `BEEPER_GPUI_HOMESERVER` to point at such a server,
and `BEEPER_GPUI_API` and `BEEPER_GPUI_BRIDGES` to point the account API and the
bridge login API at stand-ins; `BEEPER_GPUI_DATA_DIR` picks the data directory
(with a pre-seeded `session.json`, and `settings.json` for the theme). Debug desktop builds also
stand in for two Android screens: `BEEPER_GPUI_SHARE` (file paths, one per line) and
`BEEPER_GPUI_SHARE_TEXT` open the "Send to" screen as a share from another app would, and
`BEEPER_GPUI_GALLERY` (a directory) shows the attach sheet's photo grid, with
`BEEPER_GPUI_GALLERY_ACCESS` (`none`, `partial`) for its states without full access.
