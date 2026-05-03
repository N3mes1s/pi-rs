//! Implement `SandboxProvider` for a private execution backend. Per
//! RFD 0027 §1, custom sandboxes plug in through
//! `RuntimeConfig::with_sandbox_provider(...)`. When set, the runtime
//! routes every approved tool call through `execute_tool` instead of
//! dispatching inline.
//!
//! Real-world use cases:
//! - Custom microvm/container backend not covered by RFD 0023's
//!   built-in `MicroVmProvider`.
//! - Remote-exec service (RFD 0026 covers Sprites / E2B / Daytona;
//!   bring your own for an in-house equivalent).
//! - Audit-only "sandbox" that records every tool call without
//!   executing — useful for dry-run / approval-workflow scenarios.
//!
//! This example wires an audit-only sandbox that records every tool
//! call, returns a synthetic `SandboxExecution` with the recorded
//! data, and the embedder reads the recorded calls after the session
//! ends.
//!
//! Run with the `mocks` feature for the example glue:
//!     cargo run --example 05_custom_sandbox -p pi-sdk --features mocks

use async_trait::async_trait;
use pi_sdk::{
    AgentEventKind, AgentSessionRuntime, AuthStorage, ModelRegistry, RuntimeConfig,
    SandboxError, SandboxExecution, SandboxProvider, SessionManager, Settings, ToolContext,
    ToolRegistry,
};
use std::sync::Arc;
use std::sync::Mutex;

#[cfg(feature = "mocks")]
use pi_sdk::MockProvider;

/// An audit-only sandbox that records every tool call to a shared
/// vector and returns a synthetic stdout. Real implementations would
/// route the call to a microvm RPC endpoint, a container, or a remote
/// service.
struct AuditSandbox {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl SandboxProvider for AuditSandbox {
    fn name(&self) -> &'static str {
        "audit-only"
    }

    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        let entry = format!("{} cwd={} input={}", tool_name, ctx.cwd.display(), input);
        self.log
            .lock()
            .map_err(|_| SandboxError::Provider("audit log poisoned".into()))?
            .push(entry.clone());

        // Real impls return the actual stdout/stderr/exit. Audit-only
        // returns a synthetic OK so the agent loop continues.
        Ok(SandboxExecution {
            stdout: format!("[AUDIT] would run: {tool_name}"),
            stderr: String::new(),
            exit_status: 0,
        })
    }
}

#[cfg(feature = "mocks")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sandbox = Arc::new(AuditSandbox { log: log.clone() });

    let auth = AuthStorage::in_memory();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        // Use the bundled readonly tool set. The audit sandbox
        // intercepts every tool dispatch; the actual Tool impls
        // never run.
        .tools(ToolRegistry::with_readonly_extras())
        .settings(Settings {
            provider: "anthropic".into(),
            model: "test".into(),
            ..Settings::default()
        })
        .system_prompt("you are an inspector")
        .cwd(std::env::current_dir()?)
        .with_provider_factory(MockProvider::new().with_text_response("done").into_factory())
        .with_sandbox_provider(sandbox)
        .build()?;

    let runtime = AgentSessionRuntime::new(cfg);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;
    tokio::spawn(async move {
        let _ = session.prompt("List the project files.".into()).await;
    });
    while let Some(evt) = rx.recv().await {
        if matches!(evt.kind, AgentEventKind::TurnComplete) {
            break;
        }
    }

    let recorded = log.lock().unwrap().clone();
    eprintln!("[audit-sandbox] {} tool calls intercepted:", recorded.len());
    for (i, entry) in recorded.iter().enumerate() {
        eprintln!("  {}. {entry}", i + 1);
    }
    Ok(())
}

#[cfg(not(feature = "mocks"))]
fn main() {
    eprintln!("this example requires the `mocks` feature: cargo run --example 05_custom_sandbox -p pi-sdk --features mocks");
    std::process::exit(1);
}
