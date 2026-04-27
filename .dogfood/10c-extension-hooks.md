You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: extension event hooks.

Step 1. In `crates/pi-coding-agent/src/extensions.rs` add:

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ExtensionHook {
        pub event: String,        // "tool_call" | "tool_result" | "assistant_message" | "user_message"
        pub executable: String,
    }

and add `#[serde(default)] pub hooks: Vec<ExtensionHook>` to `ExtensionManifest`.

Step 2. Add a `HookDispatcher` struct also in `extensions.rs`:

    pub struct HookDispatcher {
        // event → list of (extension_root_path, executable_path, timeout)
        per_event: std::collections::HashMap<String, Vec<(PathBuf, PathBuf, std::time::Duration)>>,
    }

with constructor `pub fn from_extensions(exts: &[LoadedExtension]) -> Self` that walks every extension's `hooks` and groups by event name. The `executable_path` should resolve relative paths against `extension.root` exactly like `LoadedExtension::executable_path()` does.

Add a method:

    pub async fn dispatch(&self, event: &str, payload: &serde_json::Value)

which spawns each registered hook for the matching event name, writes a single JSON line (the payload) to its stdin, awaits its exit with the configured timeout, and discards stdout/stderr (errors are logged via `tracing::warn!`). Run hooks concurrently with `futures::future::join_all`.

Step 3. Tests in `crates/pi-coding-agent/tests/extensions_hooks.rs`:
- Build a tempdir with a fake extension whose `hooks` declares one event "tool_call" pointing at a script that writes its stdin to a sentinel file in a tempdir.
- Construct a `HookDispatcher::from_extensions`, call `dispatch("tool_call", &json!({"name":"bash","input":{}}))`.
- Assert the sentinel file exists and contains the JSON.
- Test `dispatch` with an event NOT in the manifest is a no-op (no errors, no spawned processes).
- Test that a hook whose executable doesn't exist is logged but doesn't crash dispatch.

Step 4. Don't wire it into the agent loop yet — that's a separate concern. Just expose `HookDispatcher` and `from_extensions` on `pub use` in `extensions.rs`, and assert via a test that `Startup` carries the loaded extensions through unchanged.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --test extensions_hooks`

When done output: DONE.
