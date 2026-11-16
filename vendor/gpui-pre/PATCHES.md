# Patches against upstream gpui-pre 0.3.6

Upstream `LineWrapper` truncation (`should_truncate_line`, `should_truncate_line_middle`,
`truncate_line`) cuts at any `char` boundary. That splits flags, ZWJ families, keycaps and
skin tones, leaving e.g. a lone regional indicator before the ellipsis (0.3.12 is the same).

Changes, all in `src/text_system/line_wrapper.rs` and marked `murmur patch`:

- new pure fn `grapheme_cut(line, ix, round_up)` plus test `grapheme_cut_tests`
- End/Start/Middle cut indices are passed through it
- `Cargo.toml`: added `unicode-segmentation` (version already in the root Cargo.lock)
- removed `[[example]]` and `[[test]]` sections and the examples, tests, docs and Cargo.lock
  files (not vendored); `autoexamples`/`autotests`/`autobenches` are already false

Touch pinch: upstream's touch recognizer ignores a second contact, so a two-finger pinch on a
touch screen never becomes a `PinchEvent` (only trackpads send one). Changes, marked `murmur patch`:

- new `src/pinch_tracker.rs`: follows at most two contacts and turns their distance into
  `PinchEvent`s (Started, Moved with the relative change, Ended/Cancelled)
- `src/gpui.rs`: `mod pinch_tracker`
- `src/window.rs`: a `pinch_tracker` field, and every raw touch is fed to it before the gesture
  recognizer; a resulting pinch is dispatched like a trackpad pinch

Views that outlive their window: a window's focus, activation and appearance listeners live in
the window, so a view shown again in a new window (the desktop app running on with its window
closed) lost them: text fields got no caret and the view no activation. Changes, marked
`murmur patch`: `Window::hand_listeners_on` (src/window.rs) tells the focused element it lost
focus, then moves the three listener sets to `App::kept_window_listeners` (src/app.rs);
`Window::new` takes them when set. Listeners of views that went with the old window drop out on
their next call, as upstream's do.

Re-apply on upgrade: copy the new crate into this directory, repeat the above
(`grep -rn "murmur patch" vendor/gpui-pre`), bump the `gpui-pre =` pin consumers, or drop the
vendor dir and the `[patch.crates-io]` entry once upstream cuts on grapheme boundaries.

Test: upstream's own test build needs the zed workspace assets, so `cargo test` inside this
crate does not compile. Run `grapheme_cut_tests` by copying `grapheme_cut` and the test module
into a scratch crate that depends on `unicode-segmentation`.

Touch pan on a one-axis scroller: upstream `Interactivity::paint_scroll_listener` turns a horizontal
delta into vertical scroll for a container that only scrolls vertically (and the reverse), meant for
a mouse wheel. A touch pan is already axis-locked by the gesture recognizer, so a sideways swipe on
the chat list scrolled it down. Change, marked `murmur patch`, in `src/elements/div.rs`: the
cross-axis mapping applies only when the last input was not touch (`Window::last_input_was_touch`).
Mouse wheel, shift-wheel and touchpad keep upstream behaviour.

Image release: upstream frees an image's decoded copy with `remove_asset` but leaves its sprite
atlas tile until the window closes; only `App::drop_image` frees it, and that needs the
`RenderImage`, which `Image::get_render_image` would decode again if the asset is not loaded.
Change, marked `murmur patch`: `App::has_asset`, `ImageSource::is_asset_cached` and
`Image::is_asset_cached` are no longer test-only, so the app asks first and decodes nothing.
