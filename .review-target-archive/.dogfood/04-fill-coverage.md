You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

We currently sit at 42% line coverage across the workspace. We need to
push this above 80% on the pure-logic modules. Add new test files (do
NOT modify source) for the modules below.

A. `crates/pi-coding-agent/tests/renderer.rs` — exercise
   `pi_coding_agent::renderer::Transcript` thoroughly:
   - new transcript renders to a Frame containing only the trailing
     blank separator line
   - ingesting a `UserMessage` with text adds a User block
   - consecutive `AssistantTextDelta` events coalesce into one
     AssistantText block, not many
   - consecutive `AssistantThinkingDelta` events coalesce
   - `AssistantToolCall` adds a ToolCall block; `ToolResult` adds a
     ToolResult block whose `ok` flag matches `is_error`
   - `Usage` events accumulate into `usage_total`
   - `Error` events add an Error block
   - `CompactionComplete` adds a Compact block
   - `thinking_collapsed = true` replaces the thinking text with
     a `[thinking collapsed: N chars]` placeholder line
   - `tool_collapsed = true` replaces the tool body with a
     `[tool output: N lines]` placeholder line
   - `footer()` includes the model name, the in/out tokens and the
     cwd path
   - `tail(n)` returns the last n blocks (or everything when n >
     len)
   - The renderer wraps long lines to the viewport width (test with
     a known long string and small viewport).

B. `crates/pi-coding-agent/tests/cli.rs` — already exists but extend
   it with: thinking flag is rejected for non-allowed values
   (clap returns Err), `--no-builtin-tools` and `--no-tools` pass
   through, `--tools "read,bash"` parses into the Vec, `--session`
   plus `--session-dir` coexist, `-c` and `-r` are independent
   booleans.

C. `crates/pi-coding-agent/tests/cmd.rs` — `run_list` returns Ok and
   prints something on an empty packages dir; `run_config` prints
   the agent dir path. Override `PI_CODING_AGENT_DIR` to a tempdir
   for both.

D. `crates/pi-coding-agent/tests/extensions_more.rs` — extend the
   existing extensions test with: a manifest declaring two tools
   produces two Tool entries; an extension whose executable returns
   non-JSON stdout still becomes a successful ToolResult; an
   extension whose stdout is `{"output":"x","is_error":true}` is
   surfaced as an error. Test `run_command` against a script that
   echoes its argv.

E. `crates/pi-ai/tests/anthropic_more.rs` — drive the streaming
   server through a tool_use end-to-end (content_block_start with
   tool_use, content_block_delta with input_json_delta, content_block_stop,
   message_delta with stop_reason=tool_use, message_stop). Assert
   `.generate(...)` returns one ToolCall.

F. `crates/pi-tools/tests/grep_find_ls.rs` — extend coverage:
   grep with a glob filter that matches one of two files; find
   honoring max_results; ls in a non-existent directory returns
   is_error.

G. `crates/pi-coding-agent/tests/picker_props.rs` — proptest:
   for any non-empty list of strings, picker.move_down N times
   then picker.move_up N times returns to selected = 0 (modulo).

H. `crates/pi-coding-agent/tests/keymap_more.rs` — extend coverage:
   `Keymap::defaults` includes a binding for Action::Submit and
   Action::Quit; `chord_from_event` correctly maps Tab, BackTab,
   F-keys, and arrow keys; `parse_chord("Alt+Shift+Backspace")`
   returns the right modifier combo.

After writing, run `cargo test --workspace` and iterate until
everything passes. Then run `cargo llvm-cov --workspace --summary-only
2>&1 | tail -5` and report the TOTAL line. Do not modify any non-test
file.

When done, output: DONE.
