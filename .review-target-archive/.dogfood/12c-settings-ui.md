You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: interactive `/settings` slash command in the TUI.

Background:
- `Settings` lives in `crates/pi-agent-core/src/settings.rs` with
  fields: `provider, model, thinking, steering_mode,
  follow_up_mode, transport, theme, scoped_models, ...`.
- `Settings::save(path)` already persists.

Changes:

1. New module `crates/pi-coding-agent/src/settings_ui.rs` exposing:

       pub struct SettingsField {
           pub name: &'static str,
           pub current: String,
           pub options: Vec<String>,
       }

       pub fn fields(settings: &Settings, themes: &[String]) -> Vec<SettingsField>

   Returns one field per editable setting. Options:
   - `thinking`: ["off","low","medium","high"]
   - `steering_mode`: ["one-at-a-time","all"]
   - `follow_up_mode`: ["one-at-a-time","all"]
   - `transport`: ["sse","websocket","auto"]
   - `scoped_models`: ["false","true"]
   - `theme`: themes argument

   And:

       pub fn apply(settings: &mut Settings, field: &str, value: &str) -> Result<(), String>

   Mutates the matching field. Reject unknown field/value pairs.

2. In `crates/pi-coding-agent/src/modes/interactive.rs`, replace
   the existing `/settings` arm (which currently prints the path)
   with an inline picker flow:
   - first picker: choose a field name (from `settings_ui::fields`).
   - second picker: choose a value from that field's options.
   - on Enter of value, call `settings_ui::apply` and persist via
     `Settings::save(&settings_path)`.
   - the in-memory `startup.runtime_config.settings` (and the
     session's runtime if applicable) is updated immediately.

   This is the trickiest wiring — keep it minimal: even just
   handling the field-then-value picker in run_tui's slash arm
   suffices. If reaching into the session's runtime is awkward,
   it's OK to require a restart for some fields (theme is fine
   live-applied; thinking can be set via `session.set_thinking`).

3. Add a `settings_path()` helper in
   `crates/pi-coding-agent/src/context.rs` returning the global
   settings.json path (probably already exists as
   `settings_paths().0`).

4. Tests in `crates/pi-coding-agent/tests/settings_ui.rs`:
   - `fields` returns the expected field names + options
   - `apply("thinking", "high")` mutates Settings.thinking to High
   - `apply("scoped_models", "true")` mutates the bool
   - `apply("theme", "dark")` mutates the theme name
   - `apply("unknown", "x")` returns Err
   - `apply("thinking", "extreme")` returns Err

   You don't have to integration-test the picker flow.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test settings_ui`

When done output: DONE.
