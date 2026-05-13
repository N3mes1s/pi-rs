You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: `@filename` fuzzy completion in the TUI editor.

The existing `View` struct lives in
`crates/pi-coding-agent/src/modes/interactive.rs`. The picker
infrastructure in `crate::picker::Picker<T>` is already used by
`/resume`, `/model`, `/tree`, `/fork`, `/clone` — reuse it for `@`.

Changes:

1. Add a free helper in `modes/interactive.rs` (or a new sibling
   `modes/at_completion.rs` if you prefer):

       pub(crate) fn build_at_candidates(cwd: &Path) -> Vec<PathBuf>

   It walks `cwd` with the `ignore::WalkBuilder` (already in
   pi-tools' deps; pull it in here too) honouring `.gitignore`,
   limits to 5000 paths, returns paths relative to `cwd`.

2. Add to `View`:

       pub at_active: bool,
       pub at_query_start: Option<usize>,  // byte index in editor.text where '@' was typed

3. In `handle_key`, after the existing keymap dispatch but before
   the catch-all character insert: if the user types `@`, set
   `at_active = true`, record the editor cursor as
   `at_query_start`, and open a `PickerOverlay` populated with
   `build_at_candidates(cwd)` mapped to `String` labels. Use a
   new `PickerKind::AtCompletion`. The picker_outcome arm for
   `AtCompletion` returns a new `KeyOutcome::AtComplete { picked: String }`.

   Subsequent keys while `at_active` is true should also flow
   through the picker (push_query, pop_query, Up/Down, Enter,
   Esc) — but they should also append/remove from the editor's
   text so the user sees `@<query>` in place. On Enter:
   replace the `@<query>` token in the editor with the picked
   path. On Esc: keep the literal `@<query>` text and clear the
   picker.

4. Tests in `crates/pi-coding-agent/tests/tui_at_completion.rs`:
   - `build_at_candidates` honours `.gitignore` (set up a tempdir
     with one ignored and one tracked file, assert only the
     tracked one appears).
   - typing `@s` opens the picker (View.picker is Some) with
     query "s" and at_active=true.
   - selecting a candidate via Enter replaces `@<query>` with the
     chosen path in the editor buffer.
   - Esc closes the picker but leaves `@<query>` literal text in
     the buffer.
   - typing `@` again after a previous completion opens a fresh
     picker.

   Drive the tests through `handle_key` with synthesised
   `KeyEvent`s. Use the `View::new(...)` constructor; pass a
   pre-computed candidate list directly for the picker test by
   constructing the picker yourself if `build_at_candidates`
   needs the real filesystem (you can use `tempfile::tempdir`).

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test tui_at_completion`

When done output: DONE.
