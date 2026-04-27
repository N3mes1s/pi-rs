You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Three smaller features:

1. **`--prompt-template <name|@path>`** — already parsed in
   `cli.rs` as `prompt_template: Option<String>`. Honour it:
     - if it starts with `@`, treat the rest as a path; read it
       and use the body as the prompt (after `{{args}}`
       interpolation with the joined positional args).
     - otherwise treat it as a template name; look it up in
       `startup.prompts` and render with the joined positionals
       passed as `{{args}}` and `{{ARGS}}`.
   The resolved string takes precedence over the positional
   prompt in print/json modes; in TUI mode it's pre-filled into
   the editor.
   Tests in `crates/pi-coding-agent/tests/prompt_template_resolve.rs`:
     - `@/tmp/x.md` reads the file
     - bare name resolves through PromptRegistry
     - `{{args}}` is replaced
     - missing template returns a clear error.

2. **Better `/resume` and `/tree` picker labels.** Right now the
   picker shows raw session IDs. Update the label format to:
       "<short_id>  <provider>/<model>  <YYYY-MM-DD HH:MM>  <first_user_text|...>"
   For `/tree`, label each entry with kind + short text. Implement
   helpers `pi_coding_agent::picker::format_session_label(meta:
   &SessionMeta) -> String` and
   `pi_coding_agent::picker::format_tree_entry(entry: &SessionEntry)
   -> String`. Add tests asserting the formatting handles missing
   title/empty user text and ISO timestamps correctly.

3. **Interactive `/settings`** — instead of just printing the
   path, build a tiny picker over the current `Settings` fields
   and let the user toggle bools / pick from enums. Specifically:
     - thinking: off / low / medium / high
     - steering_mode: one-at-a-time / all
     - follow_up_mode: one-at-a-time / all
     - transport: sse / websocket / auto
     - scoped_models: bool
     - theme: any installed theme name
   On Enter, persist the change via `Settings::save(...)` and
   reflect it live in the running TUI.
   Add a `SettingsPicker` enum-of-pickers in
   `pi-coding-agent::settings_ui` (new module). Pure-logic
   tests cover the field-cycle + save call, mocking the file
   write through a temp path.

After implementing:
- `cargo build --workspace`
- `cargo test --workspace --no-fail-fast`
- `bash scripts/coverage.sh 2>&1 | tail -3` ≥ 90%

When done, output: DONE.
