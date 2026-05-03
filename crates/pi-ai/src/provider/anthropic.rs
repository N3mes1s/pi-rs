use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::AuthMethod;
use crate::message::{ContentBlock, Role, ThinkingLevel};
use crate::registry::{ModelInfo, ProviderConfig};
use crate::stream::{StreamEvent, StreamEventKind};
use crate::{AiError, GenerateRequest, Result};

use super::anthropic_stream::{handle_anthropic_event, ToolAcc};
use super::{EventStream, Provider};

pub struct AnthropicProvider {
    pub config: ProviderConfig,
    pub auth: AuthMethod,
    pub client: Client,
}

pub use crate::cost::{compute_cost, UsageAcc};

impl AnthropicProvider {
    pub fn new(config: ProviderConfig, auth: AuthMethod) -> Self {
        Self {
            config,
            auth,
            client: crate::http::streaming_client_or_default(),
        }
    }

    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    fn auth_token(&self) -> Result<&str> {
        match &self.auth {
            AuthMethod::ApiKey { value } => Ok(value),
            AuthMethod::OAuth { access_token, .. } => Ok(access_token),
            AuthMethod::None => Err(AiError::MissingAuth(self.config.name.clone())),
        }
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User | Role::Tool => "user",
        Role::Assistant => "assistant",
        Role::System => "user",
    }
}

pub fn content_blocks_to_anthropic(blocks: &[ContentBlock]) -> Value {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => out.push(json!({"type": "text", "text": text})),
            ContentBlock::Thinking { text, signature } => {
                // Anthropic rejects `thinking` blocks whose `signature`
                // is null/missing with `messages.*.content.*.thinking
                // .signature.str: Input should be a valid string`. The
                // signature is only available when we received it from
                // a fresh stream (signature_delta). For replayed
                // sessions or any thinking we synthesised ourselves,
                // skip the block entirely — the upstream model has not
                // signed it, so it isn't sendable. Local renderers
                // (flamegraph, picker) still see it on disk; only the
                // outgoing request drops it.
                if let Some(sig) = signature {
                    out.push(json!({
                        "type": "thinking",
                        "thinking": text,
                        "signature": sig,
                    }));
                }
            }
            ContentBlock::ToolUse { id, name, input } => out.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => out.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            })),
            ContentBlock::Attachment { attachment } => match &attachment.kind {
                crate::message::AttachmentKind::Image { mime, base64 } => out.push(json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": mime, "data": base64},
                })),
                crate::message::AttachmentKind::File { mime, base64, name } => out.push(json!({
                    "type": "document",
                    "source": {"type": "base64", "media_type": mime, "data": base64},
                    "name": name,
                })),
            },
        }
    }
    Value::Array(out)
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn auth(&self) -> &AuthMethod {
        &self.auth
    }

    async fn discover_models(&self) -> Result<Vec<crate::registry::ModelInfo>> {
        super::discover::anthropic(&self.client, &self.config, &self.auth).await
    }

    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream> {
        let token = self.auth_token()?.to_string();
        let url = format!("{}/v1/messages", self.config.base_url);
        let messages: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                json!({
                    "role": role_str(m.role),
                    "content": content_blocks_to_anthropic(&m.content),
                })
            })
            .collect();

        let mut body = json!({
            "model": model.id,
            "max_tokens": req.max_output_tokens.unwrap_or(model.max_output_tokens),
            "messages": messages,
            "stream": true,
        });
        if let Some(sys) = req.system {
            body["system"] = Value::String(sys);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.input_schema,
                        })
                    })
                    .collect(),
            );
        }
        if !matches!(req.thinking, ThinkingLevel::Off) {
            let fragments = body_thinking_fields(req.thinking, &model.id);
            if let Some(obj) = fragments.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
            }
        }

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &token)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AiError::Provider { status, body });
        }

        let byte_stream = resp.bytes_stream();
        let event_stream = byte_stream.eventsource();
        let tool_inputs: ToolAcc = std::collections::HashMap::new();

        let model_owned = model.clone();
        let s = stream::unfold(
            (
                event_stream,
                tool_inputs.clone(),
                false,
                UsageAcc::default(),
            ),
            move |(mut es, mut acc, mut done, mut usage_running)| {
                let model_owned = model_owned.clone();
                async move {
                    let _ = &mut done;
                    if done {
                        return None;
                    }
                    loop {
                        let idle = crate::http::streaming_idle_timeout();
                        let polled = if idle.is_zero() {
                            Ok(es.next().await)
                        } else {
                            tokio::time::timeout(idle, es.next()).await
                        };
                        let item = match polled {
                            Ok(n) => n,
                            Err(_) => {
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::Error {
                                        message: format!(
                                            "anthropic: SSE stream idle for {}s — \
                                             provider stopped sending events",
                                            idle.as_secs()
                                        ),
                                    })),
                                    (es, acc, true, usage_running),
                                ));
                            }
                        };
                        let item = match item {
                            Some(v) => v,
                            None => break,
                        };
                        let ev = match item {
                            Ok(ev) => ev,
                            Err(e) => {
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::Error {
                                        message: e.to_string(),
                                    })),
                                    (es, acc, true, usage_running),
                                ));
                            }
                        };
                        let data: Value = match serde_json::from_str(&ev.data) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let etype = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if let Some(kind) = handle_anthropic_event(
                            etype,
                            &data,
                            &mut acc,
                            &mut usage_running,
                            &model_owned,
                        ) {
                            let terminal = matches!(etype, "message_stop");
                            if terminal {
                                done = true;
                                let _ = done;
                            }
                            return Some((
                                Ok(StreamEvent::new(kind)),
                                (es, acc, terminal, usage_running),
                            ));
                        }
                    }
                    None
                }
            },
        );

        let _ = tool_inputs;
        Ok(Box::pin(s))
    }
}

fn uses_adaptive_thinking(model_id: &str) -> bool {
    matches!(
        model_id,
        "claude-opus-4-7" | "claude-opus-4-6" | "claude-opus-4-5" | "claude-sonnet-4-6"
    ) || model_id.starts_with("claude-mythos-")
}

fn body_thinking_fields(level: ThinkingLevel, model_id: &str) -> Value {
    if matches!(level, ThinkingLevel::Off) {
        return json!({});
    }
    if uses_adaptive_thinking(model_id) {
        let effort = match level {
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            // Anthropic has no xhigh tier; clamp to "high" effort.
            ThinkingLevel::XHigh => "high",
            ThinkingLevel::Off => unreachable!(),
        };
        json!({
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": effort},
        })
    } else {
        let budget = match level {
            ThinkingLevel::Low => 4_000,
            ThinkingLevel::Medium => 16_000,
            // Anthropic budget caps at 32_000; XHigh shares the High budget.
            ThinkingLevel::High | ThinkingLevel::XHigh => 32_000,
            ThinkingLevel::Off => unreachable!(),
        };
        json!({
            "thinking": {"type": "enabled", "budget_tokens": budget},
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_thinking_model_detection() {
        assert!(uses_adaptive_thinking("claude-opus-4-7"));
        assert!(uses_adaptive_thinking("claude-opus-4-6"));
        assert!(uses_adaptive_thinking("claude-opus-4-5"));
        assert!(uses_adaptive_thinking("claude-sonnet-4-6"));
        assert!(uses_adaptive_thinking("claude-mythos-foo"));
        assert!(!uses_adaptive_thinking("claude-haiku-4-5-20251001"));
        assert!(!uses_adaptive_thinking("claude-3-7-sonnet-20250219"));
    }

    #[test]
    fn thinking_fields_adaptive_for_opus_4_7() {
        let v = body_thinking_fields(ThinkingLevel::Medium, "claude-opus-4-7");
        assert_eq!(
            v,
            json!({
                "thinking": {"type": "adaptive"},
                "output_config": {"effort": "medium"},
            })
        );
    }

    #[test]
    fn thinking_fields_legacy_for_haiku() {
        let v = body_thinking_fields(ThinkingLevel::Medium, "claude-haiku-4-5-20251001");
        assert_eq!(
            v,
            json!({
                "thinking": {"type": "enabled", "budget_tokens": 16000},
            })
        );
    }
}
