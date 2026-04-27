use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::auth::AuthMethod;
use crate::message::{FinishReason, Message, ThinkingLevel, Usage};
use crate::registry::{ModelInfo, ProviderConfig};
use crate::stream::StreamEvent;
use crate::tool::{ToolCall, ToolSpec};
use crate::{AiError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenAiCompat,
    Google,
    Bedrock,
    Azure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub thinking: ThinkingLevel,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    /// Provider-specific extras.
    #[serde(default)]
    pub extras: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub message: Message,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// All providers expose this trait. Implementations are stateless: state lives
/// in the agent loop.
#[async_trait]
pub trait Provider: Send + Sync {
    fn config(&self) -> &ProviderConfig;
    fn auth(&self) -> &AuthMethod;

    /// Non-streaming generation. Default impl collapses the stream.
    async fn generate(&self, req: GenerateRequest, model: &ModelInfo) -> Result<GenerateResponse> {
        let mut stream = self.stream(req, model).await?;
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage = Usage::default();
        let mut finish = FinishReason::Stop;
        use futures::StreamExt;
        use crate::stream::StreamEventKind as K;
        while let Some(ev) = stream.next().await {
            match ev?.kind {
                K::TextDelta { text: t } => text.push_str(&t),
                K::ThinkingDelta { text: t } => thinking.push_str(&t),
                K::ToolCallComplete { id, name, input } => {
                    tool_calls.push(ToolCall { id, name, input })
                }
                K::Usage { usage: u } => usage = u,
                K::Finish { reason } => finish = reason,
                K::Error { message } => return Err(AiError::Other(message)),
                _ => {}
            }
        }
        let mut content = Vec::new();
        if !thinking.is_empty() {
            content.push(crate::message::ContentBlock::Thinking {
                text: thinking,
                signature: None,
            });
        }
        if !text.is_empty() {
            content.push(crate::message::ContentBlock::Text { text });
        }
        for c in &tool_calls {
            content.push(crate::message::ContentBlock::ToolUse {
                id: c.id.clone(),
                name: c.name.clone(),
                input: c.input.clone(),
            });
        }
        Ok(GenerateResponse {
            message: Message {
                role: crate::message::Role::Assistant,
                content,
            },
            tool_calls,
            finish_reason: finish,
            usage,
        })
    }

    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream>;

    /// Live model discovery against the provider's `/v1/models` (or
    /// equivalent) endpoint. Default returns `AiError::Unsupported` for
    /// providers without a standard listing endpoint (Bedrock needs
    /// SigV4-signed `bedrock:ListFoundationModels`; Azure deployments are
    /// user-defined names rather than discoverable models).
    ///
    /// Returns `ModelInfo` entries with conservative defaults for
    /// `context_window` / `max_output_tokens` / cost (the bare list
    /// endpoints don't return those). Callers should merge live results
    /// on top of the static catalog so curated entries keep their cost
    /// data.
    async fn discover_models(&self) -> Result<Vec<ModelInfo>> {
        Err(AiError::Unsupported(format!(
            "{} doesn't support live model discovery",
            self.config().name
        )))
    }
}

pub mod anthropic;
pub mod azure;
pub mod bedrock;
pub mod discover;
pub mod google;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use azure::AzureOpenAiProvider;
pub use bedrock::BedrockAnthropicProvider;
pub use google::GoogleProvider;
pub use openai::{OpenAiCompatProvider, OpenAiProvider};
