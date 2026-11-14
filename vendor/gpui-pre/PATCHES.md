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

Re-apply on upgrade: copy the new crate into this directory, repeat the above
(`grep -rn "murmur patch" vendor/gpui-pre`), bump the `gpui-pre =` pin consumers, or drop the
vendor dir and the `[patch.crates-io]` entry once upstream cuts on grapheme boundaries.

Test: upstream's own test build needs the zed workspace assets, so `cargo test` inside this
crate does not compile. Run `grapheme_cut_tests` by copying `grapheme_cut` and the test module
into a scratch crate that depends on `unicode-segmentation`.
