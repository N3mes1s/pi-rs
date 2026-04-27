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

pub mod catalogue;
pub mod config;
pub mod ops;

pub use catalogue::{language_for_extension, LanguageEntry, DEFAULT_CATALOGUE};
pub use config::LspConfig;
pub use ops::LspOp;
