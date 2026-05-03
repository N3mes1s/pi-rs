//! Minimal embed example for `pi-sdk`.
//!
//! Demonstrates the SAFE-by-default `quick_start` path:
//! - `AuthStorage::in_memory()` (NO env scan, no CWE-526 risk),
//! - `ToolRegistry::with_readonly_extras()` (read/grep/find/ls only,
//!   no shell, no fs mutation),
//! - `LocalProcessProvider::with_readonly_defaults()` as sandbox.
//!
//! Embedders supplying credentials for one explicit provider do so
//! AFTER `quick_start` returns — the runtime starts with zero secrets.
//!
//! Run with:
//!     ANTHROPIC_API_KEY=sk-... cargo run --example 01_minimal -p pi-sdk

use pi_sdk::{quick_start, AgentEventKind, AuthMethod};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Build a SAFE-by-default runtime. No env scan, no shell tools,
    //    no fs-mutation tools. Embedders adding `bash` / `write` / `edit`
    //    must do so explicitly via `RuntimeConfig::builder()` — see the
    //    crate-root docs for that pattern.
    let runtime = quick_start("anthropic", "claude-haiku-4-5-20251001")?;

    // 2. Provide credentials for the one provider we'll use. The
    //    AuthStorage starts empty (`in_memory()` per Hardening §4.5 #8).
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("set ANTHROPIC_API_KEY before running"))?;
    runtime.config().auth_storage.set(
        "anthropic",
        AuthMethod::ApiKey { value: api_key },
    );

    // 3. Open a session and stream events.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;

    tokio::spawn(async move {
        let _ = session
            .prompt("List files in the current directory.".into())
            .await;
    });

    while let Some(event) = rx.recv().await {
        match event.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }

    Ok(())
}
