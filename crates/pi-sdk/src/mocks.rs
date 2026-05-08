//! Mock provider + mock sandbox provider for embedder tests.
//!
//! Per RFD 0027 §1 + Commit D. Embedders writing CI tests should not
//! be forced to (a) hit a real LLM endpoint, (b) spin up a microvm,
//! or (c) reach inside `pi-rs`'s own test crates for re-usable mocks.
//! These types are public, gated on the `mocks` feature flag so
//! production builds don't pay the binary-size cost.
//!
//! Enable via:
//!
//! ```toml
//! [dev-dependencies]
//! pi-sdk = { version = "0.1", features = ["mocks"] }
//! ```
//!
//! The mocks here are intentionally minimal — embedders with more
//! exotic needs (multi-turn dialogue trees, streaming with
//! interleaved tool calls, etc.) build their own. These cover the
//! 80% case: "I want to assert that my embedder code path produces
//! the right `AgentEvent`s for a known input/output."

use async_trait::async_trait;
use pi_ai::{
    AiError, AuthMethod, EventStream, FinishReason, GenerateRequest, ModelInfo, Provider,
    ProviderConfig, ProviderKind, StreamEvent, StreamEventKind,
};
use pi_agent_core::{ProviderFactory, RuntimeError};
use pi_sandbox::{SandboxError, SandboxExecution, SandboxProvider};
use pi_tools::ToolContext;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

// ─── MockProvider ────────────────────────────────────────────────

/// Stub LLM provider. By default emits one `AssistantText` event
/// containing a fixed string then `Finish::Stop`. Customise via
/// `with_canned_turns` for multi-turn or tool-call scenarios.
///
/// Provider config is set to `provider=anthropic, kind=Anthropic`
/// for compatibility with `Settings::default()`. Override via
/// `with_provider_kind` / `with_provider_name` if your test asserts
/// on the provider config.
///
/// Per RFD 0027 §3 (blanket `#[non_exhaustive]`): private fields
/// are sufficient — the struct is constructible only via
/// `MockProvider::new()` plus the chained `with_*` setters, never
/// via struct literal from outside the SDK.
#[derive(Clone)]
pub struct MockProvider {
    cfg: ProviderConfig,
    canned: Arc<StdMutex<Vec<Vec<StreamEvent>>>>,
    text_response: Arc<StdMutex<Option<String>>>,
}

impl MockProvider {
    /// New mock provider that emits one canned text response then stops.
    pub fn new() -> Self {
        Self {
            cfg: ProviderConfig {
                name: "anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: "mock".into(),
                auth_header: "x-api-key".into(),
                auth_format: "{token}".into(),
                models: vec![],
            },
            canned: Arc::new(StdMutex::new(Vec::new())),
            text_response: Arc::new(StdMutex::new(Some(String::new()))),
        }
    }

    /// Configure the provider name (default "anthropic").
    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.cfg.name = name.into();
        self
    }

    /// Configure the provider kind (default `ProviderKind::Anthropic`).
    pub fn with_provider_kind(mut self, kind: ProviderKind) -> Self {
        self.cfg.kind = kind;
        self
    }

    /// Replace the default text-response with a fixed string. The
    /// provider emits one `AssistantText { text }` followed by
    /// `Finish::Stop`. Subsequent turns repeat the same response.
    pub fn with_text_response(self, text: impl Into<String>) -> Self {
        *self.text_response.lock().unwrap() = Some(text.into());
        self
    }

    /// Use canned per-turn `StreamEvent` lists instead of the simple
    /// text-response shape. Each call to `stream()` consumes one
    /// element from the front of the list.
    ///
    /// When the canned list is exhausted, the provider emits
    /// `Finish::Stop` only.
    pub fn with_canned_turns(self, turns: Vec<Vec<StreamEvent>>) -> Self {
        *self.canned.lock().unwrap() = turns;
        *self.text_response.lock().unwrap() = None;
        self
    }

    /// Wrap this provider in a `MockProviderFactory` for use as a
    /// `RuntimeConfig::with_provider_factory(...)` argument.
    pub fn into_factory(self) -> Arc<MockProviderFactory> {
        Arc::new(MockProviderFactory { inner: self })
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn config(&self) -> &ProviderConfig {
        &self.cfg
    }

    fn auth(&self) -> &AuthMethod {
        static N: AuthMethod = AuthMethod::None;
        &N
    }

    async fn stream(
        &self,
        _req: GenerateRequest,
        _model: &ModelInfo,
    ) -> Result<EventStream, AiError> {
        // Prefer canned turns when present.
        let canned_turn = {
            let mut g = self.canned.lock().unwrap();
            if !g.is_empty() {
                Some(g.remove(0))
            } else {
                None
            }
        };
        let turn: Vec<StreamEvent> = match canned_turn {
            Some(t) => t,
            None => {
                let text = self.text_response.lock().unwrap().clone();
                let mut events = Vec::new();
                if let Some(text) = text {
                    if !text.is_empty() {
                        events.push(StreamEvent::new(StreamEventKind::TextDelta { text }));
                    }
                }
                events.push(StreamEvent::new(StreamEventKind::Finish {
                    reason: FinishReason::Stop,
                }));
                events
            }
        };
        Ok(Box::pin(futures::stream::iter(turn.into_iter().map(Ok))))
    }
}

/// Trivial `ProviderFactory` wrapping a `MockProvider`. Embedders
/// install via `RuntimeConfig::builder().with_provider_factory(...)`.
///
/// Per RFD 0027 §3 (blanket `#[non_exhaustive]`): private fields keep
/// the struct unconstructible from outside the SDK except via
/// `MockProvider::into_factory()`.
pub struct MockProviderFactory {
    inner: MockProvider,
}

impl ProviderFactory for MockProviderFactory {
    fn build(
        &self,
        _cfg: ProviderConfig,
        _auth: AuthMethod,
    ) -> Result<Box<dyn Provider>, RuntimeError> {
        Ok(Box::new(self.inner.clone()))
    }
}

// ─── MockSandboxProvider ─────────────────────────────────────────

/// Stub sandbox provider. Records every `execute_tool` call and
/// returns a configurable `SandboxExecution` (default: empty stdout,
/// exit 0). Embedders use this to assert that their tool surface
/// dispatches correctly without spinning up a real microvm.
///
/// Per RFD 0027 §3 (blanket `#[non_exhaustive]`): private fields keep
/// the struct unconstructible from outside the SDK except via
/// `MockSandboxProvider::new()` plus the chained `with_*` setters.
#[derive(Clone)]
pub struct MockSandboxProvider {
    response_stdout: Arc<StdMutex<String>>,
    response_stderr: Arc<StdMutex<String>>,
    response_exit: Arc<StdMutex<i32>>,
    calls: Arc<StdMutex<Vec<MockSandboxCall>>>,
    error_on_next: Arc<StdMutex<Option<SandboxError>>>,
}

/// Recorded `execute_tool` invocation against a `MockSandboxProvider`.
///
/// Per RFD 0027 §3 (blanket `#[non_exhaustive]`): marked so that
/// future fields (call_id, cwd, timestamp, etc.) can be added
/// MINOR-additively. Constructed only by the SDK runtime; embedders
/// receive these via `MockSandboxProvider::calls()`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MockSandboxCall {
    pub tool_name: String,
    pub input: serde_json::Value,
}

impl MockSandboxProvider {
    pub fn new() -> Self {
        Self {
            response_stdout: Arc::new(StdMutex::new(String::new())),
            response_stderr: Arc::new(StdMutex::new(String::new())),
            response_exit: Arc::new(StdMutex::new(0)),
            calls: Arc::new(StdMutex::new(Vec::new())),
            error_on_next: Arc::new(StdMutex::new(None)),
        }
    }

    /// Return this stdout on every subsequent `execute_tool` call.
    pub fn with_stdout(self, stdout: impl Into<String>) -> Self {
        *self.response_stdout.lock().unwrap() = stdout.into();
        self
    }

    /// Return this stderr on every subsequent `execute_tool` call.
    pub fn with_stderr(self, stderr: impl Into<String>) -> Self {
        *self.response_stderr.lock().unwrap() = stderr.into();
        self
    }

    /// Return this exit status on every subsequent `execute_tool` call.
    /// Exit != 0 maps to `ToolResult::is_error = true` in the runtime.
    pub fn with_exit_status(self, exit: i32) -> Self {
        *self.response_exit.lock().unwrap() = exit;
        self
    }

    /// Cause the next `execute_tool` call to return this error
    /// instead of the canned `SandboxExecution`. Cleared after one
    /// use; re-arm by calling again.
    pub fn fail_next_call(&self, err: SandboxError) {
        *self.error_on_next.lock().unwrap() = Some(err);
    }

    /// Snapshot of every recorded `execute_tool` call so far.
    pub fn calls(&self) -> Vec<MockSandboxCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Clear the recorded call list.
    pub fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }
}

impl Default for MockSandboxProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SandboxProvider for MockSandboxProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn execute_tool(
        &self,
        _ctx: &ToolContext,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        self.calls.lock().unwrap().push(MockSandboxCall {
            tool_name: tool_name.to_string(),
            input: input.clone(),
        });
        if let Some(err) = self.error_on_next.lock().unwrap().take() {
            return Err(err);
        }
        Ok(SandboxExecution {
            stdout: self.response_stdout.lock().unwrap().clone(),
            stderr: self.response_stderr.lock().unwrap().clone(),
            exit_status: *self.response_exit.lock().unwrap(),
            round_trip_ms: None,
            cost_usd: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_request() -> GenerateRequest {
        GenerateRequest {
            model: "test".into(),
            system: None,
            messages: vec![],
            tools: vec![],
            thinking: pi_ai::ThinkingLevel::default(),
            temperature: None,
            max_output_tokens: None,
            extras: serde_json::Value::Null,
        }
    }

    fn empty_model_info() -> ModelInfo {
        ModelInfo {
            provider: "anthropic".into(),
            id: "test".into(),
            alias: None,
            context_window: 1024,
            max_output_tokens: 1024,
            tier: 1,
            supports_thinking: false,
            supports_tools: true,
            supports_vision: false,
            input_cost_per_mtok: 0.0,
            output_cost_per_mtok: 0.0,
            cache_read_cost_per_mtok: None,
            cache_write_cost_per_mtok: None,
            api_kind: pi_ai::ApiKind::default(),
        }
    }

    #[test]
    fn mock_provider_default_config_is_anthropic() {
        // Config-shape smoke check (the existing assertion).
        let p = MockProvider::new();
        let cfg = p.config().clone();
        assert_eq!(cfg.name, "anthropic");
        assert!(matches!(cfg.kind, ProviderKind::Anthropic));
    }

    #[tokio::test]
    async fn mock_provider_default_emits_finish_only() {
        // Per code-review finding #5: actually drive the stream and
        // assert the "finish only" claim, not just the config shape.
        use futures::StreamExt;
        let p = MockProvider::new();
        let mut stream = p.stream(empty_request(), &empty_model_info()).await.expect("stream");
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        assert_eq!(
            events.len(),
            1,
            "default MockProvider should emit exactly one event (Finish), got {events:?}"
        );
        assert!(
            matches!(events[0].kind, StreamEventKind::Finish { .. }),
            "default MockProvider's only event should be Finish, got {:?}",
            events[0].kind
        );
    }

    #[tokio::test]
    async fn mock_provider_with_text_response_emits_text_then_finish() {
        use futures::StreamExt;
        let p = MockProvider::new().with_text_response("hi");
        let mut stream = p
            .stream(empty_request(), &empty_model_info())
            .await
            .expect("stream");
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].kind, StreamEventKind::TextDelta { .. }));
        assert!(matches!(events[1].kind, StreamEventKind::Finish { .. }));
    }

    #[tokio::test]
    async fn mock_sandbox_records_calls_and_returns_canned() {
        let s = MockSandboxProvider::new()
            .with_stdout("hello world")
            .with_exit_status(0);
        let ctx = ToolContext {
            cwd: std::env::current_dir().unwrap(),
            ..Default::default()
        };
        let _ = s
            .execute_tool(&ctx, "read", &serde_json::json!({"path": "/tmp/x"}))
            .await
            .expect("execute_tool");
        let calls = s.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "read");
        assert_eq!(calls[0].input["path"], "/tmp/x");
    }

    #[tokio::test]
    async fn mock_sandbox_fail_next_call_returns_error_once() {
        let s = MockSandboxProvider::new().with_stdout("ok");
        s.fail_next_call(SandboxError::Timeout);
        let ctx = ToolContext {
            cwd: std::env::current_dir().unwrap(),
            ..Default::default()
        };
        // First call: error.
        let r = s.execute_tool(&ctx, "read", &serde_json::json!({})).await;
        assert!(matches!(r, Err(SandboxError::Timeout)));
        // Second call: back to canned response.
        let r = s.execute_tool(&ctx, "read", &serde_json::json!({})).await;
        assert!(r.is_ok());
        // Both got recorded.
        assert_eq!(s.calls().len(), 2);
    }
}
