Vendored copy of `egui_commonmark_backend` 0.19.0 (MIT OR Apache-2.0), wired in
through `[patch.crates-io]` in `app/Cargo.toml`.

Changes: `src/elements.rs` has two `_f32` literal fixes for a rustc future-compat warning and clamps `desired_width` of code blocks to `ui.available_width()`. `src/misc.rs` sets `break_anywhere = true` for code blocks and clamps `max_width` to `ui.available_width()`. The other change is in `src/misc.rs` (`Style::to_richtext` and the `BOLD_FAMILY`
const): strong text and headings use the `"bold"` font family that `main.rs`
registers (Inter SemiBold), instead of only a brighter colour. Upstream has no
hook for this. Drop the patch and this folder when upgrading if upstream adds one.
