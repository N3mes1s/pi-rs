//! Native LSP integration scaffolding (D1).
//!
//! This module ports the *types and config* of upstream pi's
//! `native/lsp/` package: 11 operations (diagnostics, definition,
//! type_definition, implementation, references, hover, symbols, rename,
//! code_actions, status, reload), a default language-server catalogue,
//! and the pre-write hook flags (format-on-write, diagnostics-on-write).
//!
//! The actual JSON-RPC transport — spawning a server with `tokio::process`,
//! framing `Content-Length` headers, multiplexing requests across
//! `initialize`/`textDocument/didOpen`/`textDocument/didChange`/etc. —
//! is intentionally NOT implemented here. That layer is several hundred
//! lines and risky to ship without integration tests against real
//! servers; this scaffolding is the deterministic, fully-tested
//! foundation it will plug into.
//!
//! See `dogfood-blocked.md` at the repo root for the deferred work.
//!
//! D1.transport status: the JSON-RPC stdio transport (framing, id
//! correlation, initialize handshake, server-pushed notification
//! channel) lives in [`transport`]. The engine + agent-facing tool
//! still want for follow-ups; `tests/fake_lsp_server.py` is the test
//! harness that keeps us off real `rust-analyzer` in CI.

pub mod catalogue;
pub mod config;
pub mod ops;
pub mod transport;

pub use catalogue::{language_for_extension, LanguageEntry, DEFAULT_CATALOGUE};
pub use config::LspConfig;
pub use ops::LspOp;
pub use transport::{LspClient, ServerMessage, TransportError};
