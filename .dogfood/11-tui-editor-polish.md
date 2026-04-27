You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: polish the TUI editor with the three remaining UX features:

1. **`@filename` fuzzy completion.** When the user types `@`
   followed by characters in the editor, open the existing
   `Picker<PathBuf>` overlay populated with files from cwd
   (respect `.gitignore` via the `ignore` crate, which is already
   a dependency of pi-tools). The query field of the picker
   echoes what the user types after `@`. On Enter, replace the
   `@<query>` token in the editor with the chosen path. On Esc,
   keep the literal `@<query>` text and close the picker.

   Build the candidate list lazily on first `@` keypress and
   cache it for the rest of the session. Use a cap of 5000 paths
   to keep ranking snappy.

   Add the supporting state to `View` (e.g. an `at_query: Option<String>`
   plus the cached `Vec<PathBuf>`). Mutations live in the pure
   `handle_key`; the actual file walk runs in a small helper
   `pub(crate) fn build_at_candidates(cwd: &Path) -> Vec<PathBuf>`
   that's unit-testable.

   Tests in `crates/pi-coding-agent/tests/tui_at_completion.rs`:
   - typing `@s` opens the picker with query "s"
   - the path "src/lib.rs" outranks "tests/foo.rs" for query "lib"
   - selecting a candidate replaces `@<query>` with the path
   - Esc keeps the original `@<query>` text and closes picker
   - `build_at_candidates` honours `.gitignore`

2. **`!command` and `!!command` execution.** When the editor
   buffer starts with `!` or `!!` and the user submits, run the
   command via `bash -lc` capture stdout/stderr and:
     - `!command`  → append the output to the user prompt and
                     submit it as a normal user turn.
     - `!!command` → just clear the editor and don't submit
                     anything to the model (silent).
   Add `KeyOutcome::BangCommand { command, silent }` and route it
   through `run_tui` / `run_line_based`. Use the existing
   `Editor::special_command()` helper for parsing.

   Tests in `crates/pi-coding-agent/tests/tui_bang.rs`:
   - `!echo hi` produces a Submit containing "hi"
   - `!!echo hi` produces a silent outcome (no Submit)
   - editor is cleared after a bang execution
   - leading whitespace before the bang is tolerated

3. **Theme hot-reload polled in the TUI render loop.** The TUI
   already takes a snapshot of `ThemeRegistry` at startup. Switch
   it to use `HotThemes` (already in `pi-coding-agent::themes`).
   On every render tick (50 ms), call `hot.snapshot()` and re-look-up
   the active theme by name. If the theme name's RGB changed,
   the next frame re-renders with the new colours.

   Don't add I/O tests for the watcher; instead add
   `crates/pi-coding-agent/tests/themes_hot.rs` that:
     - constructs a `HotThemes` over a tempdir
     - writes a theme.json and asserts the snapshot picks it up
       within 200 ms (poll loop with timeout)
     - rewrites the file with new colours and asserts the
       snapshot reflects them.

After implementing, build, test, run coverage. Iterate until
green and TOTAL ≥ 90% on lines + functions.

Do NOT touch `runtime.rs` or any pi-ai/pi-tools source. The work
all lives in `pi-coding-agent`.

When done, output: DONE.
