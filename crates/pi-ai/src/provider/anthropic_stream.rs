//! Shared Anthropic SSE event handler used by both
//! `provider::anthropic` and `provider::bedrock` (RFD 0015).
//!
//! The Anthropic Messages-on-Bedrock wire format is byte-identical to the
//! direct Anthropic API stream, so both providers route every chunk
//! through `handle_anthropic_event` to keep parsing logic in one place.

use std::collections::HashMap;

use serde_json::Value;

use crate::cost::UsageAcc;
use crate::message::FinishReason;
use crate::registry::ModelInfo;
use crate::stream::StreamEventKind;

/// Tool-call accumulator: `index -> (id, name, json_buffer)`.
pub(super) type ToolAcc = HashMap<u32, (String, String, String)>;

/// Parse a single Anthropic SSE event. Returns `Some(StreamEventKind)`
/// when the event should be yielded to the caller, or `None` when the
/// event was state-only (e.g. updating `usage_running`).
pub(super) fn handle_anthropic_event(
    etype: &str,
    data: &Value,
    acc: &mut ToolAcc,
    usage_running: &mut UsageAcc,
    model: &ModelInfo,
) -> Option<StreamEventKind> {
    match etype {
        "message_start" => {
            if let Some(u) = data.get("message").and_then(|m| m.get("usage")) {
                usage_running.input_tokens = u
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                usage_running.cache_read_tok = u
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                usage_running.cache_write_tok = u
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
            }
            Some(StreamEventKind::MessageStart)
        }
        "content_block_start" => {
            let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let block = data.get("content_block").cloned().unwrap_or(Value::Null);
            if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                acc.insert(idx, (id.clone(), name.clone(), String::new()));
                return Some(StreamEventKind::ToolCallStart { id, name });
            }
            None
        }
        "content_block_delta" => {
            let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let delta = data.get("delta").cloned().unwrap_or(Value::Null);
            match delta.get("type").and_then(|v| v.as_str()) {
                Some("text_delta") => {
                    let t = delta
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(StreamEventKind::TextDelta { text: t })
                }
                Some("thinking_delta") => {
                    let t = delta
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(StreamEventKind::ThinkingDelta { text: t })
                }
                Some("input_json_delta") => {
                    let partial = delta
                        .get("partial_json")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Some((id, _, buf)) = acc.get_mut(&idx) {
                        buf.push_str(&partial);
                        return Some(StreamEventKind::ToolInputDelta {
                            id: id.clone(),
                            partial_json: partial,
                        });
                    }
                    None
                }
                _ => None,
            }
        }
        "content_block_stop" => {
            let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if let Some((id, name, buf)) = acc.remove(&idx) {
                let input = if buf.is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&buf).unwrap_or(Value::Null)
                };
                return Some(StreamEventKind::ToolCallComplete { id, name, input });
            }
            None
        }
        "message_delta" => {
            if let Some(usage) = data.get("usage") {
                usage_running.output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(usage_running.output_tokens);
                let final_usage = usage_running.into_usage(model);
                return Some(StreamEventKind::Usage { usage: final_usage });
            }
            if let Some(reason) = data
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str())
            {
                let r = match reason {
                    "tool_use" => FinishReason::ToolUse,
                    "end_turn" => FinishReason::Stop,
                    "max_tokens" => FinishReason::Length,
                    "refusal" => FinishReason::Refusal,
                    _ => FinishReason::Other,
                };
                return Some(StreamEventKind::Finish { reason: r });
            }
            None
        }
        "message_stop" => Some(StreamEventKind::Finish {
            reason: FinishReason::Stop,
        }),
        _ => None,
    }
}
