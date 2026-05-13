You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: polish the /export command's HTML output and bring overall
coverage back ≥ 90% on the testable surface.

Step 1. Improve `/export` output. The current implementation in
`crates/pi-coding-agent/src/modes/interactive.rs` (`export_html`
function) produces a single `<pre>` block. Replace it with a
proper HTML document:

- Each user, assistant, tool_call, tool_result, and compaction
  block becomes its own `<div class="block role-X">` with a
  `<header>` showing the role and (for tool_call) the tool name.
- Wrap message bodies in `<pre>` so whitespace is preserved.
- Include a small embedded CSS block at the top with role-coloured
  borders matching the active theme (cyan for user, green for
  assistant, yellow for tool, dark grey for thinking, red for
  errors). Use plain hex colors from `theme::ColorSpec::Rgb` if
  the theme has them, or fall back to named CSS colours.
- HTML-escape the bodies (`<`, `>`, `&`, `"`) to prevent injection.
- Title is `pi-rs session <short_id>`.

Move the renderer to `crates/pi-coding-agent/src/share.rs` (which
already has `render_session_markdown`) — add
`render_session_html(messages: &[Message], session_id: &str,
provider: &str, model: &str) -> String`. Then `interactive.rs`'s
`/export` arm calls it.

Tests in `crates/pi-coding-agent/tests/share_html.rs`:
- output starts with `<!doctype html>` and contains the session id
- HTML escaping: a message containing `<script>` produces
  `&lt;script&gt;` in the output
- each role gets its own div class
- empty session still produces a valid document

Step 2. Run `bash scripts/coverage.sh 2>&1 | tail -3`. Identify any
modules below 90% lines and add targeted test files
(`<module>_extra2.rs` if `_extra.rs` already exists). Iterate
until the TOTAL line is ≥ 90% AND functions ≥ 90%.

Likely targets to focus on if they slipped:
- `share.rs` (you'll be adding to it)
- `settings_ui.rs` (newly added)
- `interactive.rs` (newly added handle_at and handle_bang paths)

Build clean: `cargo build --workspace`
Tests green: `cargo test --workspace --no-fail-fast`
Coverage: `bash scripts/coverage.sh 2>&1 | tail -3` ≥ 90%/90%

When done output: DONE.
