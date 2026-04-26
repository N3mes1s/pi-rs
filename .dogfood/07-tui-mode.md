You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: replace the current line-based interactive REPL with a proper
raw-mode TUI built on what already exists in the workspace:

- `pi_tui::DiffRenderer` (differential renderer, DEC 2026 sync output)
- `pi_tui::Editor` (multi-line buffer with cursor, supports
  Shift+Enter newline, !command/!!command bang prefixes)
- `pi_coding_agent::renderer::Transcript` (event-to-Frame converter)
- `pi_coding_agent::keymap::{Keymap, Action, chord_from_event}`
- `pi_coding_agent::picker::Picker` (fuzzy picker)

Only modify `crates/pi-coding-agent/src/modes/interactive.rs`. Do not
touch any other source file unless you hit a missing helper that
genuinely belongs in the renderer/keymap module — in that case make
the smallest change.

Implementation requirements:

1. **Auto-detect TTY.** If stdin or stdout is not a TTY (use
   `std::io::IsTerminal`), fall back to the existing line-based
   loop (keep that code as a private function `run_line_based`).

2. **Raw mode + alternate screen.** Enter via crossterm:
       crossterm::terminal::enable_raw_mode()
       crossterm::execute!(stdout, EnterAlternateScreen, Hide)
   Always restore on exit (use a guard struct with Drop).

3. **Event loop.** Use `crossterm::event::EventStream` with tokio.
   Drive three concurrent sources:
   - terminal key events
   - agent events from the session's mpsc receiver
   - a periodic 50ms tick for cursor blink / animation

   Use `tokio::select!` over them.

4. **Editor pane below the transcript.** Draw the transcript first,
   then a 1-line separator, then the editor with its cursor. The
   editor should support:
   - Printable chars: insert
   - Backspace, Delete, Left/Right arrows, Home/End
   - Up/Down: navigate command history (a Vec<String>)
   - Shift+Enter: insert newline (multi-line input)
   - Enter: submit. If the buffer starts with "/", treat it as a
     slash command. Otherwise treat it as a user prompt.
   - Alt+Enter: queue follow-up (`session.enqueue(...)`) — display
     a small "queued: N" hint in the footer.
   - Ctrl+C: clear editor; second Ctrl+C within 1s quits.
   - Ctrl+D: quit if editor is empty.
   - Escape: if a turn is in progress, abort it (`session.abort()`);
     otherwise no-op.
   - Ctrl+L: cycles to next model (use the registry's models list).
   - Shift+Tab: cycle thinking level off→low→medium→high→off.
   - Ctrl+O: toggle `transcript.tool_collapsed`.
   - Ctrl+T: toggle `transcript.thinking_collapsed`.
   - Ctrl+G: spawn `$VISUAL` or `$EDITOR` on a tempfile, read the
     result back into the editor.

5. **Slash commands** — handled inline. At minimum:
   `/help` lists all commands. `/quit` exits cleanly. `/compact [args]`
   triggers `session.compact_with_llm(args)`. `/model <name>` calls
   `session.set_model(provider, model)`. `/tree`, `/resume`, `/fork`
   open a picker overlay (next requirement). `/clone` duplicates
   the current branch (call session_manager and create a new
   session). `/share` runs `gh gist create` via bash; if `gh` is
   missing, print a friendly error. `/login` calls
   `pi_ai::oauth::build_authorize_url` for Anthropic, prints the
   URL, then `listen_for_callback` on `127.0.0.1:54545`,
   `exchange_code`, and stores the token via `auth.set("anthropic",
   …)`.

6. **Picker overlay.** When `/resume`, `/model`, `/tree`, or `/fork`
   is invoked, switch input mode to `Picker`. The picker takes over
   the bottom half of the screen: query field on top, ranked list
   underneath, ↑/↓ to move, Enter to select, Esc to cancel. Use
   `pi_coding_agent::picker::Picker<T>`.

7. **Footer line** (single row, at the bottom): shows
   `model · in:N out:N $X.XXXX · cwd:… · queued:N · thinking:level`.

8. **Resize.** Listen for `Event::Resize(cols, rows)` and update
   `DiffRenderer::resize` and the editor's max-width.

9. **Don't render at every keystroke** — coalesce. After any state
   change, set a `dirty=true` flag; the 50ms tick redraws if dirty.

10. **Footer-bar uses the active theme** — use `startup.themes` and
    `startup.settings.theme` to pick the Theme; pass it into
    `Transcript::render` and `Transcript::footer`.

After implementing, build (`cargo build -p pi-coding-agent`) and run
the line-based fallback path with stdin redirection to ensure the
test/JSON/RPC modes still work:

    echo "hi" | ./target/debug/pi --no-context-files --no-session \
        --provider anthropic --model claude-haiku-4-5-20251001 \
        --no-tools -p "say OK"

(That's print mode and shouldn't touch the new TUI code at all.)

Then write a UNIT test for the parts that don't need a real TTY:
- A `View` struct that contains a `Transcript`, a `Keymap`, a
  `Picker` slot, and an editor history. Test that `Action::Quit`
  followed by `Action::Cancel` resets the quit-confirm timer.
- Test: applying a sequence of `KeyEvent`s through a pure
  `handle_key` function moves the editor cursor, inserts chars,
  and triggers `Submit` correctly.
- Test: opening a picker, typing a query, and pressing Enter
  resolves to the right value.

Place the unit tests inside `mod tests {}` at the bottom of the
file. Build to a clean state. Output: DONE.
