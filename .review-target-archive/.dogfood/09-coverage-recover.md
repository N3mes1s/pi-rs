You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

After the recent /clone /share /scoped-models /login + raw-TUI work,
total coverage on the testable surface dropped from 90.58% to 89.83%
(from `bash scripts/coverage.sh`). The gaps that need test coverage:

A. `crates/pi-coding-agent/src/share.rs` — currently 80.49% lines,
   50% functions. Add tests in
   `crates/pi-coding-agent/tests/share_extra.rs`. Cover whichever
   public functions are missing: render_session_markdown for
   user/assistant/tool/tool-result branches, the gh-missing
   friendly fallback (mock by overriding PATH to a tempdir without
   a `gh` binary), and any helper that builds the markdown body.

B. `crates/pi-agent-core/src/settings.rs` — dropped to 88%. The
   `save()` method is new. Add `crates/pi-agent-core/tests/settings_save.rs`:
   - save → load round-trip preserves every field including
     `scoped_models`
   - save creates parent directories
   - save returns Err on unwritable path (use a non-existent root
     that can't be created).

C. `crates/pi-agent-core/src/session.rs` — added `clone_branch`.
   Existing test in `session_clone.rs` covers the happy path. Add
   `crates/pi-agent-core/tests/session_clone_extra.rs`:
   - clone of a session with multiple Tool/ToolResult interleaved
   - clone of a session with a Compaction entry
   - clone of an unknown session returns Err.

D. The `modes/interactive.rs` file currently has unit tests inside
   `mod tests {}`. Update `pi-rs/scripts/coverage.sh` to NOT
   exclude that file — the TUI loop's pure logic (`View`,
   `handle_key`, `picker_outcome`, etc.) is testable. Keep the
   exclusion for the other modes/* files (print, json, rpc).
   Then add more tests in `crates/pi-coding-agent/tests/tui_view.rs`
   that drive `View`/`handle_key` through every branch:
   - Submit with non-slash buffer returns Submit
   - Submit with /xyz returns SlashCommand
   - QueueFollowup increments `queued_count`
   - Picker open/close, query typing, Enter selecting
   - History Up/Down navigation
   - Quit confirm timer (two Ctrl+C within 1s → Quit, otherwise clear)
   - Esc with no turn → no-op; Esc with turn_in_progress → Abort
   - Toggle thinking cycles off→low→medium→high→off
   - Toggle tool/thinking collapse flags

   Mark the `View`, `KeyOutcome`, and `handle_key` items `pub`
   (they are likely already `pub(crate)`; in that case add a thin
   re-export gated on `#[cfg(any(test, feature = "test-tui"))]`
   in `lib.rs` so external integration tests can reach them; OR
   move the unit tests INSIDE `interactive.rs` as `#[cfg(test)]
   mod tests` — that's simpler and matches what's already there.
   Use the simpler path.)

E. `crates/pi-coding-agent/src/cmd.rs` — at 58%. The remaining
   uncovered branches are `run_install` for `npm:` and `git:`
   specs (both spawn external commands). Add a test that uses an
   override-able command runner if the code allows; otherwise,
   add a test that asserts the error returned for a `git:` spec
   pointing at a file:// URL that doesn't exist (which exercises
   the spawn-then-fail path at least).

F. `pi-ai/src/provider/openai.rs` 70.77% — there are still
   uncovered branches in the streaming tool-call accumulator.
   Add `crates/pi-ai/tests/openai_stream_more.rs` covering: tool
   call with the function name and arguments arriving in a
   *single* chunk (no need for split-args reassembly); usage
   block arriving without `completion_tokens_details` (just the
   prompt/completion totals).

After writing, run:

    cargo test --workspace --no-fail-fast
    bash scripts/coverage.sh 2>&1 | tail -5

and iterate until the TOTAL line is ≥ 90% on lines AND functions.
Output: DONE.
