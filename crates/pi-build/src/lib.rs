//! `pi-build` — compile a TOML manifest into a standalone Rust
//! binary embedding pi-sdk (RFD 0028).
//!
//! Commit A scope: manifest schema (`manifest`), error type
//! (`error`), and two-pass parser + semantic validator (`parse`).
//! The `pi-build validate <toml>` CLI verb lives in `bin/pi-build.rs`.

pub mod error;
pub mod manifest;
pub mod parse;

pub use error::ManifestError;
pub use manifest::{
    AgentMeta, Manifest, ProviderConfig, ProviderName, RuntimeConfig, SecretsConfig,
    ThinkingLevel, ToolsConfig, KNOWN_TOOLS, UNSAFE_TOOLS,
};
pub use parse::{parse, validate};
