# Patches against upstream gpui-pre-linux 0.3.6

Notification click raise: upstream's Wayland `activate()` always requests its own
xdg-activation token with a stale serial, which KWin's focus stealing prevention rejects, so
clicking a desktop notification only flashed the taskbar entry. The notification server sends
its own token (`ActivationToken` signal) just before `ActionInvoked`; this lets the app hand it
over. Changes, marked `murmur patch`:

- `src/linux.rs`: a static `NEXT_ACTIVATION_TOKEN` (one `Option<String>`, latest wins),
  `pub fn set_next_activation_token` and `take_next_activation_token`
- `src/gpui_linux.rs`: re-exports `set_next_activation_token`
- `src/linux/wayland/window.rs` `activate()`: a stored token is taken and passed straight to
  `activation.activate(token, surface)`; without one, upstream behaviour. X11 ignores the token
- removed `Cargo.lock`, `Cargo.toml.orig` and `.cargo-ok` (not vendored); the crate ships no
  examples, tests or docs and `autoexamples`/`autotests`/`autobenches` are already false

Re-apply on upgrade: copy the new crate into this directory, repeat the above
(`grep -rn "murmur patch" vendor/gpui-pre-linux`), bump the `gpui-pre-linux =` pin in the root
Cargo.toml together with gpui-pre, or drop the vendor dir and the `[patch.crates-io]` entry once
upstream can activate a window with a foreign token.
