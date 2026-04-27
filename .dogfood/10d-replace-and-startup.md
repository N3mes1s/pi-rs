You are working inside the pi-rs Cargo workspace at /home/user/Playground/pi-rs.

Goal: extension manifest field for replacing built-in tools, and an
async startup hook.

Step 1. In `crates/pi-coding-agent/src/extensions.rs`:
- Add `#[serde(default)] pub replaces_builtin: Vec<String>` to `ExtensionManifest`.
- Add `#[serde(default)] pub startup_executable: Option<String>` to `ExtensionManifest`.

Step 2. In `crates/pi-coding-agent/src/startup.rs`:
- Before registering extension tools, walk every loaded extension's `replaces_builtin` list and call `tools.unregister(&name)` on the ToolRegistry to remove the matching builtin, BEFORE registering the extension's same-named tool. This way `extensions::extension_tools(&loaded_exts)` runs *after* the builtins were stripped.
- For each loaded extension whose `startup_executable` is set, run it via the existing `tokio::process::Command` plumbing with the extension's `timeout_ms` (or 30_000 if not set), discarding stdout/stderr. The Result is just logged with `tracing::warn!` on failure — never crashes startup. Since `assemble` is sync, use `tokio::runtime::Handle::current().block_on(...)` ONLY IF you're sure we're inside a runtime; otherwise spawn via `std::thread::spawn` + `tokio::runtime::Runtime::new`. Simpler: make `assemble` async — it's only called from `bin/pi.rs` inside `#[tokio::main]`. Update the call site accordingly.

Step 3. Tests in `crates/pi-coding-agent/tests/extensions_replace.rs`:
- Build a fake extension whose manifest declares `replaces_builtin: ["bash"]` and exports a tool also named `bash` (just a stub).
- Build a `ToolRegistry::with_defaults()` and assert it has a `bash` tool that is the builtin.
- Apply the replacement logic (extract the relevant snippet into a free function `extensions::apply_replacements(reg: &mut ToolRegistry, exts: &[LoadedExtension])` so it's unit-testable without driving the whole startup).
- Assert that `bash` now points at the extension tool (call its spec().name and confirm) — and that no duplicate registration happened.

Step 4. Tests in `crates/pi-coding-agent/tests/extensions_startup.rs`:
- Build a fake extension whose `startup_executable` writes a sentinel file in a tempdir.
- Add a free function `extensions::run_startup_hooks(exts: &[LoadedExtension]) -> impl Future<Output=()>` (async).
- Run it and assert the sentinel exists.
- Test that a missing executable just logs a warning and doesn't panic.

Build clean: `cargo build --workspace`
Tests green: `cargo test -p pi-coding-agent --tests`

When done output: DONE.
