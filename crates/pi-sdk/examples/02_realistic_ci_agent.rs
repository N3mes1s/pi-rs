//! Realistic embed shape per RFD 0027 §5b. Once an embedder gets past
//! Hello World they need: tool-event surfacing, cost tracking,
//! structured errors, cancellation, explicit auth.
//!
//! This is the example most production embedders should copy first.
//!
//! Run with:
//!     MY_CI_ANTHROPIC_KEY=sk-... cargo run --example 02_realistic_ci_agent -p pi-sdk

use pi_sdk::{
    cost::{estimate_cost_usd, CostRegistry},
    AgentEventKind, AgentSessionRuntime, AuthStorage, LocalProcessProvider, ModelRegistry,
    RuntimeConfig, SessionManager, Settings, ToolRegistry,
};
use std::sync::Arc;
use tokio::sync::mpsc;

const BUDGET_USD: f64 = 0.50;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Explicit auth — no env scan in production. Embedder names the
    //    keys it trusts. (Pre-Commit-H5 readers note: from_env_explicit
    //    is the H5 deliverable; this example is post-H5.)
    let auth = AuthStorage::from_env_explicit([("anthropic", "MY_CI_ANTHROPIC_KEY")])?;

    // 2. Build the runtime config explicitly. Use the safe-by-default
    //    readonly tool set + LocalProcessProvider for this CI inspector.
    //    For an agent that needs to write or shell out, swap in
    //    `with_unsafe_extras()` and a real microvm/remote sandbox.
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::with_readonly_extras())
        .settings(Settings {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            ..Settings::default()
        })
        .system_prompt("You are a CI inspector. Read the repo, do not modify it.")
        .cwd(std::env::current_dir()?)
        .with_sandbox_provider(Arc::new(LocalProcessProvider::with_readonly_defaults()))
        // H2 budget guards — defaults are 10M tokens / 64 calls; tighten for CI.
        .with_max_session_tokens(100_000)
        .with_max_tool_invocations_per_turn(20)
        .build()?;

    let runtime = AgentSessionRuntime::new(cfg);
    let registry = CostRegistry::with_bundled_defaults();
    let model_id = "claude-haiku-4-5-20251001".to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Arc::new(runtime.create_session(Some(tx))?);

    // 3. Cancellation — Ctrl-C aborts the in-flight turn cleanly.
    let abortable = session.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\n[ctrl-c] aborting session");
        abortable.abort().await;
    });

    // 4. Send a CI-shaped task in the background; main loop reads events.
    let prompt_session = session.clone();
    let prompt_handle = tokio::spawn(async move {
        prompt_session
            .prompt("Inspect the current directory and list any failing test files.".into())
            .await
    });

    let mut turn_cost: f64 = 0.0;
    while let Some(event) = rx.recv().await {
        match event.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::AssistantToolCall { call } => {
                eprintln!("\n[tool] {}", call.name);
            }
            AgentEventKind::Usage { usage } => {
                turn_cost += estimate_cost_usd(&usage, &model_id, &registry);
                if turn_cost > BUDGET_USD {
                    eprintln!(
                        "\n[budget] turn cost ${turn_cost:.4} > cap ${BUDGET_USD}; aborting"
                    );
                    session.abort().await;
                }
            }
            AgentEventKind::TurnComplete => break,
            AgentEventKind::Error { message } => {
                eprintln!("\n[error] {message}");
                break;
            }
            _ => {}
        }
    }

    eprintln!("\n[cost] turn total: ${turn_cost:.4}");

    // 5. The structured prompt result is also available (for embedders
    //    that want to react to per-prompt success/failure independent
    //    of the event stream).
    match prompt_handle.await? {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("\n[runtime] {e:?}");
            Err(format!("agent error: {e}").into())
        }
    }
}
