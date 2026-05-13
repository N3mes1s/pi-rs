You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

The crates `pi-tools` and `pi-agent-core` already have a few tests in
their `tests/` directories. The crates `pi-tui` and `pi-coding-agent`
have none. Your job is to bring all four crates close to >90% line
coverage on deterministic code, by adding new test files only.

Constraints:

- Do NOT make real network calls. The `wiremock` dev-dependency is
  already wired into `pi-coding-agent` and `pi-ai`; it is fine to add
  it to other crates if needed (already in the workspace dep set).
- Do NOT modify any non-test source file unless a test reveals an
  actual bug; in that case, fix the bug and add a single comment in
  the test explaining what was wrong.
- Use `tempfile::tempdir()` for any filesystem-backed test.
- For terminal rendering, render into a `Vec<u8>` writer and assert on
  bytes; do NOT require a real TTY.

Add the following test files (one test per concern, named clearly):

A. `crates/pi-tools/tests/timeout_and_truncate.rs`
   - bash tool times out cleanly when sleep > timeout_ms
   - read tool truncates very large files via the model_output cap
   - read tool returns image attachments as base64 with the right mime

B. `crates/pi-agent-core/tests/compaction.rs`
   - Compactor::default keeps the last 6 user turns and produces a
     non-empty summary block when there are more
   - Compactor with keep_last_turns=0 keeps no historical messages
   - LlmCompactor produces a context_recap message when given a
     mock provider that returns a fixed summary string. (Implement
     a tiny in-process Provider in the test that returns a canned
     GenerateResponse and counts its calls.)
   - settings::Settings::merge_project overlays a project file on
     top of a global file
   - context::discover_context_files finds AGENTS.md in cwd and
     parents

C. `crates/pi-tui/tests/`
   - `editor.rs`: insert/backspace/clear/special_command (! and !!)
   - `theme.rs`: ThemeRegistry::new has dark+light, install/get/names
     work, ColorSpec round-trips through serde
   - `renderer.rs`: DiffRenderer::new + render(empty frame) emits no
     bytes; render(["hello"]) writes "hello"; resize() updates width

D. `crates/pi-coding-agent/tests/`
   - `slash.rs`: parse() recognises "/foo bar baz" with name="foo"
     and args="bar baz"; render_template substitutes both {{args}}
     and {{ARGS}}; SlashRegistry::new contains the built-in commands;
     register_templates adds template-derived ones
   - `prompts.rs`: PromptRegistry::load_dir picks up *.md files and
     ignores others; render fills in {{vars}}
   - `skills.rs`: SkillRegistry::load_dir reads SKILL.md in
     subdirectories AND bare *.md files; the description is the
     first non-blank non-heading line
   - `picker.rs`: empty query returns items in original order;
     fuzzy matching ranks results; move_up/move_down wrap
   - `themes.rs`: load_themes picks up JSON files; HotThemes can be
     constructed in a tempdir and snapshot returns the loaded set
   - `keymap.rs`: chord_from_event lowercases char codes; lookup
     returns the bound action; merge_overrides replaces an action's
     binding instead of adding a duplicate
   - `extensions.rs`: discover() walks both nested-extension dirs
     AND a single-extension root (manifest at root); ExtensionTool
     converts JSON output to ToolResult correctly. Use a tiny shell
     script as the executable: `#!/bin/sh` followed by
     `printf '{"output":"hi","is_error":false}\n'`. Make it +x with
     std::fs::set_permissions.
   - `cli.rs`: Cli::effective_mode follows priority order
     rpc>json>print>interactive; prompt_text joins positionals; at_files
     extracts @-prefixed paths
   - `packages.rs`: discover() reads package.json manifests, including
     the convention-based directories; package_dirs() returns
     extension/skill/prompt/theme dirs from both the manifest and the
     conventional locations.

After writing, run for each crate:

    cargo test -p pi-tools
    cargo test -p pi-agent-core
    cargo test -p pi-tui
    cargo test -p pi-coding-agent

If any test fails, fix the test to match real behaviour OR fix the
underlying code if it is genuinely broken. Re-run until all green.

When done, output: DONE.
