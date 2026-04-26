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

pub mod cli;
pub mod cmd;
pub mod context;
pub mod extensions;
pub mod keymap;
pub mod modes;
pub mod picker;
pub mod renderer;
pub mod packages;
pub mod prompts;
pub mod sdk;
pub mod share;
pub mod skills;
pub mod slash;
pub mod startup;
pub mod telemetry;
pub mod themes;

pub use cli::Cli;
