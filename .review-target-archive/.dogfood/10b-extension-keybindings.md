You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: extension-registered keybindings.

Step 1. In `crates/pi-coding-agent/src/extensions.rs`, add:

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ExtensionKeybinding {
        pub chord: String,
        pub command: String,
    }

and add `#[serde(default)] pub keybindings: Vec<ExtensionKeybinding>` to `ExtensionManifest`.

Step 2. In `crates/pi-coding-agent/src/keymap.rs`, add a parallel storage on `Keymap`:

    pub extension_bindings: BTreeMap<Chord, (usize, String)>

(That is: chord → (extension_index, command_name).) Don't change the existing `Action` enum. Add a method `bind_extension(&mut self, chord: &str, ext_idx: usize, command_name: String)` that uses `parse_chord` and inserts. Add `lookup_extension(&self, ev: &KeyEvent) -> Option<(usize, String)>` mirroring `lookup`.

Step 3. In `crates/pi-coding-agent/src/startup.rs`, after extensions are loaded, walk every loaded extension's `keybindings` list and call `keymap.bind_extension(&kb.chord, ext_idx, kb.command.clone())`. Persist on `Startup::keymap` as before.

Step 4. In `crates/pi-coding-agent/src/modes/interactive.rs` `handle_key`: when an event has no `Action` match, fall back to `keymap.lookup_extension(ev)` — if it returns `Some((idx, name))`, return a new `KeyOutcome::ExtensionCommand { extension_index: idx, command_name: name, args: String::new() }`. Add that variant to `KeyOutcome`. The TUI dispatcher then calls `extensions::run_command` and surfaces stdout as a Note block. (Just adding the variant + lookup is enough — wiring the dispatch into run_tui's match is a one-line addition.)

Step 5. Tests in `crates/pi-coding-agent/tests/extensions_keymap.rs`:
- `Keymap::bind_extension("Ctrl+B", 0, "deploy".into())` then `keymap.extension_bindings` contains the right entry.
- `lookup_extension` against a simulated `KeyEvent` for Ctrl+B returns `Some((0, "deploy"))`.
- A `KeyEvent` for an unbound chord returns `None`.
- `handle_key` returns `KeyOutcome::ExtensionCommand { extension_index: 0, command_name: "deploy", args: "" }` for the Ctrl+B event.
- An invalid chord string in `bind_extension` is silently ignored (returns false, no panic).

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test extensions_keymap`

When done output: DONE.
