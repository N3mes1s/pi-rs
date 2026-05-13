You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: extension-registered slash commands.

Step 1. In `crates/pi-coding-agent/src/slash.rs`, add a new variant:

    Extension { extension_index: usize, command_name: String }

to the `SlashKind` enum. Add `SlashRegistry::register_extension_commands(&mut self, items: &[(usize, &ExtensionCommandManifest)])` that inserts each as a `SlashCommand { kind: SlashKind::Extension { ... }, name: command_name, description: command.description }`. Import `ExtensionCommandManifest` from `crate::extensions`.

Step 2. In `crates/pi-coding-agent/src/startup.rs`, after the extensions are loaded, call `register_extension_commands` on the slash registry. Hold the slash registry on `Startup` (add a `pub slash_registry: SlashRegistry` field).

Step 3. In `crates/pi-coding-agent/src/modes/interactive.rs`, when a slash command resolves to `SlashKind::Extension { extension_index, command_name }`, call `extensions::run_command(&startup.extensions[extension_index], &command_name, &args).await` and print stdout. Wire it into both run_tui (`handle_slash` arm in the picker outcome path) and run_line_based.

Step 4. Tests in `crates/pi-coding-agent/tests/extensions_slash.rs`:
- Build a mock `LoadedExtension` with `commands: [{name: "deploy", description: "..."}]`. Register it via `register_extension_commands`. Assert `slash_registry.get("deploy")` exists and its `kind` is `Extension { extension_index: 0, command_name: "deploy" }`.
- Build an extension whose executable is a shell script that echoes its argv. Call `extensions::run_command` directly on it and assert the captured stdout contains the args.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent extensions_slash`

When done output: DONE.
