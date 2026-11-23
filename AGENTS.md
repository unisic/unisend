# Unisend

A Matrix chat client in Rust: one codebase, GPUI for the UI, matrix-sdk for Matrix, for Android (the
main target), Linux, macOS and Windows. It serves any homeserver, and Beeper accounts with their
bridges to WhatsApp, Signal, Messenger and other networks.

Privacy-first: the owner wants maximum privacy. For any feature that sends data off the device
(push, previews, GIFs, maps, analytics, crash reports, fonts, update checks), pick the option
that sends the least to the fewest parties, with no Google services and no telemetry; when a
choice trades privacy for convenience, ask the owner and state what each party would see. Logs
name files and error types; message text, names, tokens and keys stay out of them.

## Names

The product is Unisend, and every text a user reads says so. Older names are identifiers that
installed copies and run-time lookups depend on, so they stay as they are: the crate and binary
`beeper-gpui`, the environment prefix `BEEPER_GPUI_`, the data directory `beeper-gpui`, the app id
and Java package `dev.gpui.beeper`, the Keystore alias `murmur-data` (renaming it loses every
user's data key), and the `murmur patch` marks in `vendor/`.

## Shape

- `src/backend.rs` and `src/backend/`: the Matrix side, on its own thread with a tokio runtime.
- `src/ui.rs` and `src/ui/`: one GPUI view, `BeeperApp`, on the main thread.
- The seam between them is two bounded channels: `Cmd` in, `Ev` out (`src/backend.rs`). The UI
  never awaits the network and the backend never touches GPUI. A feature that needs the server is
  a `Cmd` variant with its handler, an `Ev` variant, and its arm in `BeeperApp::on_event`.
- `src/lib.rs`: both entry points (`run_desktop`, `android_main`), `Settings`, `Palette`, and the
  Android JNI helpers.
- One UI serves phone and desktop. Layout follows width: a window narrower than `NARROW` gets the
  phone layout, on any platform. `gpui_kit::is_mobile()` is a compile-time "this is Android",
  for what only a phone has. Platform code sits behind `cfg(target_os)`.
- What a bridged network accepts comes from `com.beeper.room_features` (`src/backend/features.rs`):
  an act the network cannot take is hidden, not left to fail.
- Each module opens with a `//!` paragraph that says what it is for. Find a feature's home with
  `rg '^//!' src`, and keep that paragraph true when the module changes.
- Dependencies come from crates already in `Cargo.lock`, with a comment in `Cargo.toml` saying
  where from, as the others have. A crate new to the tree needs the owner's yes.

## Platforms

A build compiles only its own platform's `cfg` blocks, so each platform needs its own proof. Read
the platform's file before changing its code or building for it.

| Platform | Status | Proof that a change compiles | Read first |
|---|---|---|---|
| Android | main target, tested on a phone | `cargo ndk` build of the lib | `docs/platforms/android.md` |
| Linux | built and tested | `cargo check` on Linux | `docs/platforms/linux.md` |
| macOS | in progress | `cargo check` on a Mac | `docs/platforms/macos.md` |
| Windows | compiles, never run | `cargo check --target x86_64-pc-windows-gnu` | `docs/platforms/windows.md` |
| iOS | no port | none | the iOS row of `docs/parity.md` |

## Read before you change

- What a screen looks like, a color, a size, a gesture: `docs/design.md`. Colors come from
  `Palette::of(cx)` and font sizes go through `fs()`.
- A text the user reads: `docs/i18n.md`. It is `t!("English")` plus its Polish in
  `src/i18n/pl.rs`, and `cargo test --lib i18n` fails on a missing one.
- A Matrix event, account data, or anything a bridge reads or writes: `docs/parity.md`, which
  records how Beeper's own apps store it, so changes stay in sync with them.
- Files on disk, keys, sign-in and sign-out: `docs/data-encryption.md`. The app lock:
  `docs/applock.md`.
- `vendor/`: the `PATCHES.md` beside the code, and add every new patch to it.
- Tests that need a homeserver, environment variables, KLIPY and update builds:
  `docs/development.md`.
- What Signal, WhatsApp, Instagram and Messenger do, and what bridges cannot carry:
  `docs/parity-apps.md`.

## Done

- `cargo test --lib` passes, and the changed code compiles for every platform whose `cfg` blocks
  it touches, by that platform's proof above.
- A platform this machine cannot build is named in the report as "unverified on <platform>".
- A running test copy uses `BEEPER_GPUI_DATA_DIR=<scratch directory>` on desktop and the
  `UNISEND_DEV=1` build on a phone: the default directory and `dev.gpui.beeper` hold the owner's
  signed-in account.
- The docs move with the code: a feature's row in `docs/parity.md`, the Privacy section of
  `README.md` when something new leaves the device, and the platform's status in its file.
- The website moves with the app, by the section below.

## Website

The site is `unisic/unisend-website`, locally `~/Projects/Uni-websites/unisend-website`
(`git pull --ff-only` there first). Its documentation is one Markdown file per page in
`content/docs/`, front matter `title`, `description`, `order`, `group`; the landing page is
`src/routes/+page.svelte`. Both describe the latest release: every fact on them is read in this
repo's code or docs at that release's tag, and what only `dev` has waits for the release.

- A change a user can see or set (a feature, a setting, a screen, a shortcut, a platform
  difference, a party that sees data): its docs page says it, by the name the UI shows, and
  under the Settings path where it lives. A feature with no page yet goes on the closest page,
  or gets a new one.
- A feature removed or changed: every line on the site that states it, found with `rg` over
  `content/docs` and `src/routes`, says the new truth or goes.
- A release: every user-visible change since the previous tag (`git log <previous>..<tag>`) is on
  the site by the two rules above; the version on the landing page and in its Discord embed, the
  version line of
  `content/docs/introduction.md`, and the file names, minimum versions and signing notes of
  `content/docs/install.md` match the release page. The landing page's feature list and
  platform table match the README.
- Done on the site: `npm run check` with 0 errors, `npm run build` passes, and every
  `/docs/` link and anchor in `build/` resolves. The site is committed in its own repo.
