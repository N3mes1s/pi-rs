//! Implement a domain-specific tool by implementing the `Tool` trait
//! and registering it. Per RFD 0027 §1, custom tools plug in alongside
//! the built-in tools through the same `ToolRegistry`.
//!
//! This example registers a `roll_dice` tool that the agent can call
//! to roll N dice with M sides each. Demonstrates:
//! - implementing the `Tool` trait;
//! - declaring a JSON-schema input;
//! - returning a `ToolResult` with structured display data;
//! - registering via the H3 explicit-result API and handling
//!   DuplicateName.
//!
//! Run with the `mocks` feature so we don't need a real LLM:
//!     cargo run --example 03_custom_tool -p pi-sdk --features mocks

use async_trait::async_trait;
use pi_sdk::{
    AgentEventKind, AgentSessionRuntime, AuthStorage, ModelRegistry, RuntimeConfig,
    SessionManager, Settings, Tool, ToolContext, ToolError, ToolRegistry, ToolResult, ToolSpec,
};
use serde_json::{json, Value};
use std::sync::Arc;

#[cfg(feature = "mocks")]
use pi_sdk::{MockProvider, MockSandboxProvider};

struct RollDiceTool;

#[async_trait]
impl Tool for RollDiceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "roll_dice".into(),
            description: "Roll N dice with M sides each. Returns the individual rolls and total."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "n":     { "type": "integer", "minimum": 1, "maximum": 100 },
                    "sides": { "type": "integer", "minimum": 2, "maximum": 1000 },
                },
                "required": ["n", "sides"],
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let n = input
            .get("n")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::InvalidInput("missing `n`".into()))?
            .min(100) as usize;
        let sides = input
            .get("sides")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::InvalidInput("missing `sides`".into()))?
            .min(1000) as u64;

        // Deterministic stub for the example — real implementation
        // would use rand::thread_rng().
        let rolls: Vec<u64> = (0..n).map(|i| (i as u64 % sides) + 1).collect();
        let total: u64 = rolls.iter().sum();

        let model_output = format!("rolled {n}d{sides}: {rolls:?} (total: {total})");
        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output,
            display: Some(json!({
                "kind": "roll_dice",
                "n": n,
                "sides": sides,
                "rolls": rolls,
                "total": total,
            })),
            is_error: false,
        })
    }
}

#[cfg(feature = "mocks")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a ToolRegistry that has *only* our custom tool — the
    // safe-by-default pattern from the production checklist.
    let mut tools = ToolRegistry::new();
    // H3: register returns Result<(), DuplicateName>. Surface the
    // collision explicitly rather than silently last-write-wins.
    tools
        .register(Arc::new(RollDiceTool))
        .map_err(|e| format!("dice tool registration failed: {e}"))?;

    // Mock provider that emits a single tool call → tool result → finish.
    // For your own embedder, swap in your real provider via
    // `with_provider_factory`.
    let mock_provider = MockProvider::new().with_text_response("dice are fair");

    let auth = AuthStorage::in_memory();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(tools)
        .settings(
            // Use a real model alias so ModelRegistry::resolve()
            // finds it. The MockProvider intercepts all actual API
            // calls, so the model id is just a registry key.
            Settings::builder()
                .provider("anthropic")
                .model("claude-haiku-4-5-20251001")
                .build(),
        )
        .system_prompt("You are a dice oracle.")
        .cwd(std::env::current_dir()?)
        .with_provider_factory(mock_provider.into_factory())
        .with_sandbox_provider(Arc::new(MockSandboxProvider::new()))
        .build()?;

    let runtime = AgentSessionRuntime::new(cfg);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;
    tokio::spawn(async move {
        // Surface prompt errors so silent failures are visible. Pre-fix
        // the underscored `let _ = ...` discarded UnknownModel.
        if let Err(e) = session.prompt("Roll 3d6.".into()).await {
            eprintln!("[error] prompt failed: {e}");
        }
    });

    while let Some(evt) = rx.recv().await {
        match evt.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::AssistantToolCall { call } => {
                eprintln!("\n[tool] {}({})", call.name, call.input);
            }
            AgentEventKind::ToolResult { result } => {
                eprintln!("[result] {}", result.model_output);
            }
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(not(feature = "mocks"))]
fn main() {
    eprintln!("this example requires the `mocks` feature: cargo run --example 03_custom_tool -p pi-sdk --features mocks");
    std::process::exit(1);
}
