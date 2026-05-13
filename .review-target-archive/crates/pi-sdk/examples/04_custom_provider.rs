//! Implement `Provider` (or `ProviderFactory`) for a hypothetical new
//! LLM service. Per RFD 0027 §1, custom providers plug in through
//! `RuntimeConfig::with_provider_factory(...)`. The runtime never
//! constructs providers itself — it asks the factory.
//!
//! Real-world use cases:
//! - Local LLM via llama.cpp or Ollama (HTTP wrapper).
//! - Internal model service with a non-standard auth scheme.
//! - Test injection (when you want stronger control than `MockProvider`).
//!
//! This example wires a deterministic Provider that always responds
//! with a fixed message, demonstrating the trait shape and the
//! `ProviderFactory::build` contract.
//!
//! Run with the `mocks` feature for the example glue:
//!     cargo run --example 04_custom_provider -p pi-sdk --features mocks

use async_trait::async_trait;
use pi_sdk::{
    AgentEventKind, AgentSessionRuntime, AiError, AuthMethod, AuthStorage, EventStream,
    FinishReason, GenerateRequest, ModelInfo, ModelRegistry, Provider, ProviderConfig,
    ProviderFactory, ProviderKind, RuntimeConfig, RuntimeError, SessionManager, Settings,
    StreamEvent, StreamEventKind, ToolRegistry,
};
use std::sync::Arc;

#[cfg(feature = "mocks")]
use pi_sdk::MockSandboxProvider;

/// A custom provider that writes a deterministic canned response.
/// Replace `stream()` with your real LLM transport (HTTP, gRPC, etc.).
#[derive(Clone)]
struct EchoProvider {
    cfg: ProviderConfig,
    response: String,
}

#[async_trait]
impl Provider for EchoProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }

    fn auth(&self) -> &AuthMethod {
        // Provider trait requires returning a reference to AuthMethod;
        // for stateless providers the canonical pattern is a static
        // AuthMethod::None.
        static NONE: AuthMethod = AuthMethod::None;
        &NONE
    }

    async fn stream(
        &self,
        _req: GenerateRequest,
        _model: &ModelInfo,
    ) -> Result<EventStream, AiError> {
        // Real impl: open an HTTP/SSE connection, parse SSE chunks,
        // map each to a StreamEvent. For this example we yield one
        // TextDelta + one Finish.
        let events = vec![
            StreamEvent::new(StreamEventKind::TextDelta { text: self.response.clone() }),
            StreamEvent::new(StreamEventKind::Finish { reason: FinishReason::Stop }),
        ];
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

/// `ProviderFactory` is the entry point the runtime calls. It receives
/// the resolved provider config (from `Settings::provider` lookup) and
/// the auth method, and returns a boxed Provider.
struct EchoFactory {
    response: String,
}

impl ProviderFactory for EchoFactory {
    fn build(
        &self,
        cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, RuntimeError> {
        Ok(Box::new(EchoProvider {
            cfg,
            response: self.response.clone(),
        }))
    }
}

#[cfg(feature = "mocks")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let auth = AuthStorage::in_memory();
    let cfg = RuntimeConfig::builder()
        .session_manager(SessionManager::in_memory())
        .auth_storage(auth.clone())
        .model_registry(ModelRegistry::new(auth))
        .tools(ToolRegistry::new())
        .settings(
            // The factory will see this provider name + ProviderKind in
            // the cfg argument. For a real service you'd populate
            // ModelRegistry with a ProviderConfig referencing your
            // service's name; here we use a real anthropic model alias
            // so ModelRegistry::resolve() finds it (the EchoFactory
            // intercepts the actual API call regardless of model id).
            Settings::builder()
                .provider("anthropic")
                .model("claude-haiku-4-5-20251001")
                .build(),
        )
        .system_prompt("you are echo")
        .cwd(std::env::current_dir()?)
        .with_provider_factory(Arc::new(EchoFactory {
            response: "hello from a custom provider".into(),
        }))
        .with_sandbox_provider(Arc::new(MockSandboxProvider::new()))
        .build()?;

    let runtime = AgentSessionRuntime::new(cfg);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = runtime.create_session(Some(tx))?;
    tokio::spawn(async move {
        if let Err(e) = session.prompt("anything".into()).await {
            eprintln!("[error] prompt failed: {e}");
        }
    });
    while let Some(evt) = rx.recv().await {
        match evt.kind {
            AgentEventKind::AssistantTextDelta { text } => print!("{text}"),
            AgentEventKind::TurnComplete => break,
            _ => {}
        }
    }
    println!();
    // Suppress unused-import on ProviderKind (it's available for embedders
    // that want to declare the variant on their custom provider).
    let _ = ProviderKind::Anthropic;
    Ok(())
}

#[cfg(not(feature = "mocks"))]
fn main() {
    eprintln!("this example requires the `mocks` feature: cargo run --example 04_custom_provider -p pi-sdk --features mocks");
    std::process::exit(1);
}
