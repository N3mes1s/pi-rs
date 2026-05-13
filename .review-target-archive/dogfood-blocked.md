# dogfood-tier1 — deferred / blocked

## D1 native LSP module — partial

Shipped in `crates/pi-coding-agent/src/native/lsp/`:

* `ops.rs` — the 11 op enum + serde + parse round-trip
* `catalogue.rs` — default language → server map (rust, typescript,
  python, go, ruby, c/cpp, json, yaml, lua, bash) plus
  `language_for_extension` lookup
* `config.rs` — `LspConfig` (master enable, format-on-write,
  diagnostics-on-write, per-language overrides)
* full unit tests for all three (16 tests, all passing)

Deferred (not shipped in this pass):

* JSON-RPC transport over `tokio::process::Command` stdio — the
  `Content-Length`-framed protocol implementation, including
  `initialize`, `textDocument/{didOpen,didChange,didSave,formatting,
  publishDiagnostics}`, and request/response correlation.
* Real implementations of the 11 ops on top of that transport.
* The `Tool` adapter that exposes `lsp_*` to the model.
* Hook into the file-write tool to call format-on-write +
  diagnostics-on-write.
* Integration tests using a vendored fake LSP server (the standard
  test pattern: spawn `node fake-lsp.js`, exchange a few RPCs, assert
  responses).

The above is a 1–2 day chunk and was descoped to keep the dogfood
shipping cadence honest. The scaffolding committed in this branch is
the deterministic, fully-tested foundation that work will plug into.
