use pi_ai::{Message, ToolCall, ToolResult, Usage};
use serde::{Deserialize, Serialize};

/// Stream of events emitted by the agent loop. Mirrors the events the
/// upstream pi UI listens to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventKind {
    SessionStarted { id: String, cwd: String, model: String, provider: String },
    UserMessage { message: Message },
    AssistantStart,
    AssistantTextDelta { text: String },
    AssistantThinkingDelta { text: String },
    AssistantToolCall { call: ToolCall },
    ToolResult { result: ToolResult },
    AssistantMessage { message: Message },
    Usage { usage: Usage },
    TurnComplete,
    Error { message: String },
    Aborted,
    /// Compaction was triggered (manual or automatic).
    CompactionStart { instructions: Option<String> },
    CompactionComplete { summary: String, freed_tokens: u64 },
    /// One notification from a `monitor` tool (RFD 0017). `lines` is one
    /// or more stdout lines joined with `\n`, batched within the
    /// `Settings::monitor::batch_window_ms` window.
    MonitorEvent {
        monitor_id: String,
        description: String,
        lines: String,
    },
    /// Emitted exactly once when a monitor exits (RFD 0017).
    MonitorEnded {
        monitor_id: String,
        description: String,
        exit_code: Option<i32>,
        cancelled: bool,
        aborted_reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: String,
    pub entry_id: String,
    pub timestamp: i64,
    pub kind: AgentEventKind,
}

pub type EventSender = tokio::sync::mpsc::UnboundedSender<AgentEvent>;
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<AgentEvent>;
