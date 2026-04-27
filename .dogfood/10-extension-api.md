You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: extend the subprocess-extension API so extensions can register
slash commands, keybindings, and event hooks — bringing pi-rs closer
to upstream pi's TypeScript extension surface while staying
Rust-idiomatic.

The current `pi-extension.json` schema (in
`crates/pi-coding-agent/src/extensions.rs`) supports `tools` and
`commands`. Extend it:

1. **Slash commands.** Manifest entries in `commands` should become
   `/<name>` slash commands in the TUI. Update
   `slash::SlashRegistry` so a new method
   `register_extension_commands(commands: &[(&LoadedExtension,
   ExtensionCommandManifest)])` adds them, with `kind:
   SlashKind::Extension { extension_index, command_name }` (add
   that variant). The interactive mode dispatches to
   `extensions::run_command(ext, name, args)` and prints the
   stdout. Add tests in
   `crates/pi-coding-agent/tests/extensions_slash.rs`.

2. **Keybindings.** Add a `keybindings` array to the manifest:
       "keybindings": [{"chord": "Ctrl+B", "command": "deploy"}]
   At startup, after building the keymap, walk every loaded
   extension's `keybindings` and register them as a NEW Action
   variant `Action::ExtensionCommand(usize, String)` — or simpler:
   keep `Action` closed and instead store extension-bound chords
   in a separate `BTreeMap<Chord, (ext_idx, command_name)>` on
   `Keymap`. Pick whichever feels more idiomatic; test the
   resolution in `crates/pi-coding-agent/tests/extensions_keymap.rs`.

3. **Event hooks.** Manifest entries in a new `hooks` array
       "hooks": [{"event": "tool_call", "executable": "./bin/audit"}]
   register a subscriber. After every relevant agent event the
   runtime should fire matching extension hooks — but the runtime
   doesn't know about extensions, so do this in the interactive
   mode's event-loop instead. Add a small helper struct
   `extensions::HookDispatcher` that takes a slice of
   `LoadedExtension`s, owns a Tokio mpsc sender per hook, and a
   `dispatch(event_kind: &str, payload: &serde_json::Value)`
   method that spawns the executable with the JSON event on stdin
   and discards stdout. Add tests using a fake hook script that
   writes the received JSON to a file in a tempdir.

   Supported events at minimum: `tool_call`, `tool_result`,
   `assistant_message`, `user_message`. Wire them into both
   `run_tui` and `run_line_based` event loops.

4. **Replacing built-in tools.** Add a manifest field
   `replaces_builtin: ["bash"]` (Vec<String>). At startup, if an
   extension declares it replaces a builtin, the loader removes
   the builtin from `ToolRegistry` BEFORE registering the
   extension's same-named tool. Test in
   `crates/pi-coding-agent/tests/extensions_replace.rs`.

5. **Async startup.** A manifest `startup_executable` field, if
   set, runs once at startup. The loader awaits its exit (with the
   `timeout_ms` cap) before continuing. Use `run_command` style.
   Test by writing a startup script that creates a sentinel file
   in a tempdir, then asserting the file exists after `assemble`.

After implementing:

- Build cleanly with `cargo build --workspace`
- `cargo test --workspace --no-fail-fast` all green
- `bash scripts/coverage.sh 2>&1 | tail -3` ≥ 90%

Do NOT modify `runtime.rs` or `lib.rs` of `pi-agent-core` for
this. Keep all extension wiring in `pi-coding-agent`.

When done, output: DONE.
