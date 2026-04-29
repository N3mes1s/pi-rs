//! pi-coding-agent — the `pi` binary plus its SDK surface.
//!
//! The library re-exports everything callers need to build a session
//! programmatically, mirroring the SDK described in upstream pi's README:
//!
//! ```ignore
//! use pi_coding_agent::sdk::*;
//!
//! # tokio_test::block_on(async {
//! let auth = AuthStorage::from_env();
//! let registry = ModelRegistry::new(auth.clone());
//! let session_mgr = SessionManager::in_memory();
//! let cfg = build_runtime_config(BuildConfig {
//!     auth,
//!     registry,
//!     session_manager: session_mgr,
//!     ..Default::default()
//! }).unwrap();
//! let (_runtime, session) = create_agent_session(cfg, None).unwrap();
//! let _ = session.prompt("hello".into()).await;
//! # });
//! ```

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
pub mod sdk;
pub mod settings_ui;
pub mod share;
pub mod skills;
pub mod slash;
pub mod slash_cost;
pub mod startup;
pub mod telemetry;
pub mod themes;

pub use cli::Cli;
