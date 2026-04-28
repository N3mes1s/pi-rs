# RFD 0001 — LSP-on-write hook

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** 369f3e4

## Summary

Wrap `pi_tools::write::WriteTool` so that, when LSP is enabled and the
written file's language is enabled, every successful write triggers
two follow-on requests: `format_on_write` (apply the language server's
formatting edits in-place) and `diagnostics_on_write` (attach pulled
diagnostics to the tool's `display` payload). Both are best-effort:
the write itself never fails because of a hook problem.

## Background

H1–H4 shipped the LSP engine, the agent-facing `lsp` tool, and the H5
step 1 settings wiring (`Settings::lsp` → `LspConfig`). What's missing
from the original `dogfood-blocked.md` punch list is the file-write
integration — the only deferred D1 item that hasn't landed.

`engine.diagnostics()` already exists. `engine.formatting()` does not;
the spec assumed it did. This RFD adds it as a thin sibling of the
existing `textDocument/*` ops.

## Proposal

### New engine op

```rust
impl LspEngine {
    /// `textDocument/formatting` — full-document formatting.
    /// Returns `TextEdit[]` (LSP 3.17 §3.17.13) verbatim.
    pub async fn formatting(&self, file: &Path) -> Result<Value, EngineError>;
}
```

Sends conventional defaults (`tabSize:4`, `insertSpaces:true`,
`trimTrailingWhitespace`, `insertFinalNewline`, `trimFinalNewlines`).
No per-file override surface this round.

### New tool wrapper

```rust
// crates/pi-coding-agent/src/native/lsp/write_tool.rs
pub struct LspWriteTool {
    inner: WriteTool,
    config: LspConfig,
    engine: OnceCell<Arc<LspEngine>>,
}
```

`spec()` forwards to the inner tool — same name (`"write"`), so
registering this in `startup.rs` after `with_extras()` overrides the
bare write tool. The wrapper is only registered when
`config.enabled == true`; the no-LSP path stays byte-identical to the
pre-H5 build.

`invoke()` flow:

1. Call `inner.invoke()` first. If it errors or LSP is disabled,
   return immediately.
2. Resolve the path through `pi_tools::resolve_path` (must be made
   `pub`). If the language isn't recognised or isn't enabled, return.
3. Lazy-init `engine` via `OnceCell` (mirrors `LspTool`'s pattern).
4. If `format_on_write`: call `engine.formatting(path)`, sort the
   `TextEdit[]` by start position **descending**, apply in-place, write
   back. Update `display.bytes` and add `display.formatted: true`.
5. If `diagnostics_on_write`: call `engine.diagnostics(path)`, accept
   either the LSP 3.17 `{ kind, items }` shape or a raw array, and
   attach `display.diagnostics`. Never flip `is_error`.

### TextEdit application

```rust
fn apply_text_edits(text: &str, edits: &[Value]) -> Option<String>;
```

Sort descending by `range.start` (line, then character). Fail closed
(`None`) on any out-of-bounds range — the file is *not* rewritten.
This protects against a buggy server returning stale offsets.

### Path-resolution visibility

Bump `pi_tools::resolve_path` from `pub(crate)` to `pub` so the
wrapper canonicalises paths the same way the inner write tool does.

## Test plan

Three layers:

1. **Unit (`apply_text_edits`)** — descending-order application,
   empty-edits no-op, out-of-bounds abort, EOF position handling.
2. **Unit (`LspWriteTool::invoke` with stub engine)** — assert the
   call sequence under each combination of `format_on_write` /
   `diagnostics_on_write`. The stub records ops; no real server.
3. **Integration (`fake_lsp_server.py`)** — write a Rust file via the
   wrapper and observe `textDocument/formatting` + `diagnostic`
   requests on the wire. Assert the on-disk content is replaced by
   the formatter's edits.

Real-rust-analyzer coverage already lives in
`tests/lsp_real_rust_analyzer.rs`; no need to extend it for this.

## Out of scope

- `textDocument/willSave` / `willSaveWaitUntil` — fire-and-forget after
  the write lands is sufficient and avoids the timeout-coordination
  cliff of synchronous formatting. (Future RFD if we ever need
  cancel-on-format-failure semantics.)
- TUI surfacing of diagnostics — they go in the tool result for now.
  The agent reads them; the user sees them via the transcript.
- Per-file formatting options — `tabSize` etc. could come from
  `LspConfig` per-language overrides later.
- Format-on-paste, format-on-type, code-action menus.

## Open questions

- **Should `display.diagnostics` always be the full `items` array, or
  only `severity:Error`?** The `dogfood-tier4-task.md` spec said
  "any `severity:Error` entries" — this RFD proposes the full list and
  lets the agent filter. Easier to debug, slightly more tokens in the
  tool result.
- **Should we expose a way for the inner write to *opt out* of the
  hook?** E.g. a tool input flag `skip_lsp: true`. Not adding one
  unless a real use case shows up.
