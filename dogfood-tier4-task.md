# dogfood-tier4 task spec — H5: LSP-on-write hooks

## Scope (single-session, ~1–2 hour bound)

Wire the LSP engine into the file-write codepath so saved files trigger
**format-on-write** and **diagnostics-on-write**, both gated by per-language
config. The engine, transport, didOpen flow, and 11 ops already exist
(commits 497f276..cb8002f). Open work:

### 1. Settings load path

`crates/pi-coding-agent/src/native/lsp/config.rs` already defines `LspConfig`
with `enabled`, `format_on_write`, `diagnostics_on_write`, plus per-language
overrides. Today `startup.rs:243-245` constructs `LspConfig::default()` (master
off). Wire the existing settings loader (search for `Settings::load` /
`load_settings_from_disk` in the codebase) so the master switch + per-language
opts come from the user's `settings.toml`.

* Decide: nested `[lsp]` table, or top-level `lsp_*` keys? Match whichever
  pattern the existing `Settings` struct uses.
* Round-trip test: serialise a non-default LspConfig into TOML, parse it
  back, confirm equality.
* Default behaviour when the section is missing must remain `enabled = false`
  (no surprise spawns).

### 2. Write-tool hook

Locate the file-write tool (`pi-tools` or `crates/pi-coding-agent/src/native/`
— search for `write_file` / a tool whose `name()` returns `"write"`). After a
successful write, if `LspConfig.format_on_write` is true and the file's
extension maps to an enabled language:

* Call `LspEngine::formatting(path)` — that op already exists; wire it.
  Apply the returned `TextEdit[]` back to the file (highest line first to
  avoid offset drift). If formatting returns null/empty, no-op.
* Then if `LspConfig.diagnostics_on_write` is true, call
  `LspEngine::diagnostics(path)` and surface any `severity:Error` entries to
  the agent (return a structured diagnostic block in the tool's `display`
  payload, not as `is_error` — diagnostics are informational, not failures).

The hook is best-effort: if rust-analyzer isn't running yet, spawn it via
the existing `prepare()` path. If spawn fails (no LSP installed), swallow the
error silently — write succeeded, hook is bonus.

### 3. Tests

* Unit: write-tool with a stubbed engine that records calls; assert
  `formatting` then `diagnostics` were invoked in order when both flags are
  on; neither when both are off; selective when only one is on.
* Integration (use the existing `fake_lsp_server.py`): write a file, observe
  the formatting/diagnostics requests on the wire, assert the file content
  was rewritten by the formatter's edits.
* Don't add a real-rust-analyzer integration test for this — the existing
  `lsp_real_rust_analyzer.rs` already exercises the underlying engine ops.

## Definition of done

* `cargo test --workspace` green
* New unit + fake-server tests proving the hook fires under config
* Settings round-trip test
* One commit per logical step (settings wiring → hook plumbing → tests)
* A `dogfood-tier4-summary.md` at the repo root listing what shipped vs.
  deferred and why

## Out of scope (defer to tier5+)

* Full LSP write-on-save flow with `textDocument/willSave` /
  `willSaveWaitUntil`. We're firing fire-and-forget after the write lands.
* TUI surfacing of diagnostics — they go in the tool result for now.
* Format-on-paste, format-on-type, code-action menu wiring.
