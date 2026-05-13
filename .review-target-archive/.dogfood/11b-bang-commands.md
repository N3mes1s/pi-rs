You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: `!command` and `!!command` execution from the TUI editor.

Background:
- The `Editor` primitive in `crates/pi-tui/src/editor.rs` already has
  a `special_command()` method returning `Option<EditorEvent>` for
  `!` (BangCommand{silent:false}) and `!!` (BangCommand{silent:true}).
- The `View::handle_key` in
  `crates/pi-coding-agent/src/modes/interactive.rs` handles the
  `Action::Submit` action that drains the editor; this is where
  bang detection should fire.

Changes:

1. Add `KeyOutcome::Bang { command: String, silent: bool }`.

2. In `handle_key`'s `Action::Submit` arm, BEFORE checking for a
   slash command (`slash::parse`), check `Editor::special_command`
   on a clone of the buffer. If it returns
   `EditorEvent::BangCommand`, return `KeyOutcome::Bang { command,
   silent }` instead of Submit.

3. In `run_tui`'s match-on-KeyOutcome and `run_line_based`'s
   equivalent, handle the `Bang` arm:
       - run `tokio::process::Command::new("bash").arg("-lc").arg(&command)`
         capturing stdout+stderr with a 30s timeout.
       - if `!silent`: feed the captured output AS THE NEXT USER PROMPT
         (i.e. append it to the editor buffer or simulate a
         `Submit(output)` flow). Concretely: call
         `session.prompt(output)` directly from the run_tui loop.
       - if `silent`: just push a `Block::Note(format!("$ {} →
         {} bytes", command, output.len()))` to the transcript and
         clear the editor.
   The editor is already cleared by the `Submit` arm before
   returning Bang — no extra clearing needed.

4. Tests in `crates/pi-coding-agent/tests/tui_bang.rs`:
   - typing `!echo hi` then Submit returns
     `KeyOutcome::Bang { command: "echo hi", silent: false }`.
   - typing `!!echo hi` returns
     `KeyOutcome::Bang { command: "echo hi", silent: true }`.
   - typing `   !ls` (leading whitespace) is still recognised.
   - editor is cleared after a bang submission.
   - typing `/help` is still routed as SlashCommand, not Bang.
   - typing `hello` is still routed as Submit.

   Drive everything through synthesised `KeyEvent`s and
   `handle_key`. Don't shell out in tests — the bang detection
   itself is the unit under test.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test tui_bang`

When done output: DONE.
