//! pi-ai — unified multi-provider LLM API.
//!
//! Wraps Anthropic Messages, OpenAI Chat Completions, and OpenAI-compatible
//! endpoints behind a single trait so the agent loop never has to care about
//! provider-specific field naming, reasoning trace formats, or token reporting
//! quirks.

pub mod auth;
pub mod cost;
pub mod discover;
pub mod message;
pub mod oauth;
pub mod provider;
pub mod registry;
pub mod stream;
pub mod tool;

pub use auth::{AuthMethod, AuthStorage};
pub use discover::{
    cache_path as discovered_cache_path, refresh_all, refresh_and_save, DiscoveredCache,
    ProviderDiscovery,
};
pub use message::{
    Attachment, AttachmentKind, ContentBlock, FinishReason, Message, Role, ThinkingLevel, Usage,
};
pub use oauth::{
    build_authorize_url, endpoints_for_provider, exchange_code, is_expired, OAuthEndpoints, Pkce,
    TokenResponse,
};
pub use provider::{
    AnthropicProvider, AzureOpenAiProvider, BedrockAnthropicProvider, GenerateRequest,
    GenerateResponse, GoogleProvider, OpenAiCompatProvider, OpenAiProvider, Provider, ProviderKind,
};
pub use registry::{ModelInfo, ModelRegistry, ProviderConfig};
pub use stream::{StreamEvent, StreamEventKind};
pub use tool::{ToolCall, ToolResult, ToolSpec};

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("missing credentials for provider {0}")]
    MissingAuth(String),
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("provider returned error {status}: {body}")]
    Provider { status: u16, body: String },
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, AiError>;
