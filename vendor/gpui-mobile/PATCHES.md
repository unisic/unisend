# Patches against upstream gpui-mobile 5fd54f4

Upstream as pinned in the root `Cargo.toml` (git rev `5fd54f4554263ae06753e90472c7f2e2ff5d7495`),
vendored without `example/`, `screenshots/`, `.github/` and `.cargo/`, which the build does not use.

System clipboard on Android: upstream's `AndroidClipboard` in `src/android/platform.rs` is an
in-process string, so Copy, Cut and Paste in gpui-kit's text fields and text selection never
reached other apps. `read` and `write` now go through `packages::clipboard` (the app's
`GpuiClipboard` Java helper, `ClipboardManager`) when the `clipboard` feature is on, and keep the
in-process string as the fallback when JNI fails. Marked `murmur patch`.

Re-apply on upgrade: copy the new crate into this directory, repeat the above
(`grep -rn "murmur patch" vendor/gpui-mobile`), bump the `gpui-mobile` rev in the root
`Cargo.toml`, or drop the vendor dir and the `[patch."https://github.com/itsbalamurali/gpui-mobile"]`
entry once upstream's platform clipboard is the system one.
