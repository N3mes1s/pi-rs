//! pi-coding-agent — the `pi` binary plus internal modules.
//!
//! Embedders integrating pi-rs into another Rust application should
//! depend on **`pi-sdk`** (the public SDK façade, RFD 0027) instead
//! of this crate. `pi-coding-agent` ships the pi binary's CLI, TUI,
//! halo loop, evolve daemon, and other binary-side modules — none
//! of which are part of the embedder contract.
//!
//! ```ignore
//! // Embedders use pi-sdk:
//! use pi_sdk::{quick_start, AgentEventKind, AuthMethod};
//!
//! # tokio_test::block_on(async {
//! let runtime = quick_start("anthropic", "claude-haiku-4-5-20251001").unwrap();
//! runtime.config().auth_storage.set(
//!     "anthropic",
//!     AuthMethod::ApiKey { value: "sk-...".into() },
//! );
//! let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
//! let session = runtime.create_session(Some(tx)).unwrap();
//! tokio::spawn(async move {
//!     let _ = session.prompt("hello".into()).await;
//! });
//! while let Some(evt) = rx.recv().await {
//!     if matches!(evt.kind, AgentEventKind::TurnComplete) { break; }
//! }
//! # });
//! ```
//!
//! See `crates/pi-sdk/README.md` and `crates/pi-sdk/examples/` for
//! the full embedder surface.

pub mod auto_approve;
pub mod autoresearch;
pub mod cli;
pub mod cmd;
pub mod context;
pub mod evolve;
pub mod extensions;
pub mod footer;
pub mod keymap;
pub mod markdown;
pub mod modes;
pub mod native;
pub mod packages;
pub mod picker;
pub mod picker_model;
pub mod prompts;
pub mod renderer;
pub mod settings_ui;
pub mod share;
pub mod skills;
pub mod slash;
pub mod slash_cost;
pub mod startup;

pub mod halo;
pub mod telemetry;
pub mod themes;

pub use cli::Cli;
