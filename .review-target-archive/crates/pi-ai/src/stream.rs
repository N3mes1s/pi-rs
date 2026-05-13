use crate::message::{FinishReason, Usage};
use serde::{Deserialize, Serialize};

/// Stream events emitted while a provider response is being received.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEventKind {
    /// New assistant message starts.
    MessageStart,
    /// A delta of plain text.
    TextDelta { text: String },
    /// A delta of reasoning text.
    ThinkingDelta { text: String },
    /// Signature for the just-completed thinking block. Anthropic emits
    /// these in `signature_delta` events; the signature must be passed
    /// back verbatim on the next turn or the API rejects the request
    /// with `messages.*.content.*.thinking.signature.str: Input should
    /// be a valid string`. Stream consumers attach the signature to the
    /// resulting `ContentBlock::Thinking`.
    ThinkingSignature { signature: String },
    /// A tool call has started; partial input may follow as JSON deltas.
    ToolCallStart { id: String, name: String },
    /// JSON-string fragment of tool call input (Anthropic-style).
    ToolInputDelta { id: String, partial_json: String },
    /// Tool call fully formed.
    ToolCallComplete {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Final usage report.
    Usage { usage: Usage },
    /// Final stop reason.
    Finish { reason: FinishReason },
    /// Error from the provider mid-stream.
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    pub kind: StreamEventKind,
}

impl StreamEvent {
    pub fn new(kind: StreamEventKind) -> Self {
        Self { kind }
    }
}
