//! `pi-build` — compile a TOML manifest into a standalone Rust
//! binary embedding pi-sdk (RFD 0028).
//!
//! Crate layout matches RFD 0028 §A.2:
//! - `manifest` — serde shape (Commit A).
//! - `error`    — ManifestError enum (Commit A).
//! - `parse`    — two-pass parser + semantic validator (Commit A).
//! - `codegen`  — pure renderer Manifest → Cargo project (Commit B).
//! - `build`    — filesystem write + cargo subprocess (Commit B/C).

pub mod build;
pub mod codegen;
pub mod error;
pub mod manifest;
pub mod parse;

pub use build::{cargo_build, write_tree, BuildError, BuildOptions, BuildOutcome};
pub use codegen::{manifest_sha256, render, RenderedTree};
pub use error::ManifestError;
pub use manifest::{
    AgentMeta, Manifest, ProviderConfig, ProviderName, RuntimeConfig, SecretsConfig,
    ThinkingLevel, ToolsConfig, KNOWN_TOOLS, UNSAFE_TOOLS,
};
pub use parse::{parse, validate};

/// pi-build's own version, used as the codegen header and as the
/// pi-sdk caret-pin source. Bound at compile time.
pub const PI_BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
