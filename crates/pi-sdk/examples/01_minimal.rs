//! Minimal embed example for `pi-sdk`.
//!
//! Sends one prompt to an Anthropic-backed agent and prints the streamed
//! response. The fully-fledged example (with cost tracking, cancellation,
//! and structured-error handling) ships in `examples/02_realistic_ci_agent.rs`
//! once Commits B-E land.
//!
//! Run with:
//!     ANTHROPIC_API_KEY=sk-... cargo run --example 01_minimal -p pi-sdk

use pi_sdk::{
    build_runtime_config, AgentEventKind, AgentSessionRuntime, AuthStorage, BuildConfig,
    LocalProcessProvider, Settings, ToolRegistry,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Wire up auth from env. Anthropic / OpenAI / Google auto-detected.
    //
    // NOTE on safety: `from_env()` slurps every supported provider's env var
    // unconditionally. Production embedders should prefer `from_env_explicit`
    // (lands in Commit H5 per RFD 0027 §4.5 #8); for this minimal demo the
    // existing seed-module shape is preserved.
    let auth = AuthStorage::from_env();

    // 2. Build a runtime config with the default tool set + a real sandbox.
    let cfg = build_runtime_config(BuildConfig {
        auth: auth.clone(),
        // Demo convenience: `with_extras()` registers eight tools —
        // read/write/edit/bash + grep/find/ls/web_search.
        // Production agents should use `ToolRegistry::new()` and register
        // tools explicitly so the surface is auditable. See RFD 0027
        // Open Question #1 for canonical guidance.
        tools: ToolRegistry::with_extras(),
        settings: Settings {
            provider: std::env::var("PI_PROVIDER")
                .unwrap_or_else(|_| "anthropic".into()),
            model: std::env::var("PI_MODEL")
                .unwrap_or_else(|_| "claude-haiku-4-5-20251001".into()),
            ..Settings::default()
        },
        ..BuildConfig::default()
    })
    .with_sandbox_provider(Arc::new(LocalProcessProvider::with_defaults()));

    // 3. Open a session.
    let runtime = AgentSessionRuntime::new(cfg);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;

    // 4. Send a prompt. Stream events back.
    tokio::spawn(async move {
        let _ = session
            .prompt("List files in the current directory and summarise.".into())
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
