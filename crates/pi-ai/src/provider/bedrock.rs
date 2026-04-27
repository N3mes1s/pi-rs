use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::AuthMethod;
use crate::message::{FinishReason, Role, ThinkingLevel, Usage};
use crate::registry::{ModelInfo, ProviderConfig};
use crate::stream::{StreamEvent, StreamEventKind};
use crate::{AiError, GenerateRequest, Result};

use super::{EventStream, Provider};

/// AWS Bedrock provider that wraps the Anthropic Messages wire format.
///
/// The base URL is region-scoped (e.g. `https://bedrock-runtime.us-east-1.amazonaws.com`).
/// The path is `/model/{model_id}/invoke-with-response-stream` and the body
/// uses `anthropic_version: "bedrock-2023-05-31"` instead of the `model` field.
///
/// For test purposes we accept a pre-signed bearer via `AuthMethod::ApiKey` so
/// wiremock tests don't need real SigV4 signing.
pub struct BedrockAnthropicProvider {
    pub config: ProviderConfig,
    pub auth: AuthMethod,
    pub client: Client,
    pub region: String,
}

impl BedrockAnthropicProvider {
    /// Create a new provider. `region` defaults to `AWS_REGION` env var or
    /// `"us-east-1"` when not set.
    pub fn new(config: ProviderConfig, auth: AuthMethod) -> Self {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        Self {
            config,
            auth,
            client: Client::new(),
            region,
        }
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
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

#[async_trait]
impl Provider for BedrockAnthropicProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn auth(&self) -> &AuthMethod {
        &self.auth
    }

    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream> {
        let token = self.auth_token()?.to_string();

        // Bedrock path: /model/{model_id}/invoke-with-response-stream
        let url = format!(
            "{}/model/{}/invoke-with-response-stream",
            self.config.base_url, model.id
        );

        // Build messages list (filter out system role — goes into top-level `system`)
        let messages: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                let role_str = match m.role {
                    Role::User | Role::Tool => "user",
                    Role::Assistant => "assistant",
                    Role::System => "user",
                };
                json!({
                    "role": role_str,
                    "content": super::anthropic::content_blocks_to_anthropic(&m.content),
                })
            })
            .collect();

        // Bedrock uses `anthropic_version` instead of `model`
        let mut body = json!({
            "anthropic_version": "bedrock-2023-05-31",
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
        match req.thinking {
            ThinkingLevel::Off => {}
            level => {
                let budget = match level {
                    ThinkingLevel::Low => 4_000,
                    ThinkingLevel::Medium => 16_000,
                    ThinkingLevel::High => 32_000,
                    _ => 0,
                };
                body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
            }
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
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
        let tool_inputs: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new();

        let s = stream::unfold(
            (event_stream, tool_inputs, false),
            move |(mut es, mut acc, mut done)| async move {
                let _ = &mut done;
                if done {
                    return None;
                }
                while let Some(item) = es.next().await {
                    let ev = match item {
                        Ok(ev) => ev,
                        Err(e) => {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::Error {
                                    message: e.to_string(),
                                })),
                                (es, acc, true),
                            ));
                        }
                    };
                    let data: Value = match serde_json::from_str(&ev.data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let etype = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match etype {
                        "message_start" => {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::MessageStart)),
                                (es, acc, false),
                            ));
                        }
                        "content_block_start" => {
                            let idx =
                                data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let block =
                                data.get("content_block").cloned().unwrap_or(Value::Null);
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
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::ToolCallStart {
                                        id,
                                        name,
                                    })),
                                    (es, acc, false),
                                ));
                            }
                        }
                        "content_block_delta" => {
                            let idx =
                                data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let delta = data.get("delta").cloned().unwrap_or(Value::Null);
                            match delta.get("type").and_then(|v| v.as_str()) {
                                Some("text_delta") => {
                                    let t = delta
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    return Some((
                                        Ok(StreamEvent::new(StreamEventKind::TextDelta {
                                            text: t,
                                        })),
                                        (es, acc, false),
                                    ));
                                }
                                Some("thinking_delta") => {
                                    let t = delta
                                        .get("thinking")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    return Some((
                                        Ok(StreamEvent::new(StreamEventKind::ThinkingDelta {
                                            text: t,
                                        })),
                                        (es, acc, false),
                                    ));
                                }
                                Some("input_json_delta") => {
                                    let partial = delta
                                        .get("partial_json")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if let Some((id, _, buf)) = acc.get_mut(&idx) {
                                        buf.push_str(&partial);
                                        return Some((
                                            Ok(StreamEvent::new(StreamEventKind::ToolInputDelta {
                                                id: id.clone(),
                                                partial_json: partial,
                                            })),
                                            (es, acc, false),
                                        ));
                                    }
                                }
                                _ => {}
                            }
                        }
                        "content_block_stop" => {
                            let idx =
                                data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            if let Some((id, name, buf)) = acc.remove(&idx) {
                                let input = if buf.is_empty() {
                                    Value::Object(serde_json::Map::new())
                                } else {
                                    serde_json::from_str(&buf).unwrap_or(Value::Null)
                                };
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::ToolCallComplete {
                                        id,
                                        name,
                                        input,
                                    })),
                                    (es, acc, false),
                                ));
                            }
                        }
                        "message_delta" => {
                            if let Some(usage) = data.get("usage") {
                                let u = Usage {
                                    output_tokens: usage
                                        .get("output_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0),
                                    ..Default::default()
                                };
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::Usage { usage: u })),
                                    (es, acc, false),
                                ));
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
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::Finish { reason: r })),
                                    (es, acc, false),
                                ));
                            }
                        }
                        "message_stop" => {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::Finish {
                                    reason: FinishReason::Stop,
                                })),
                                (es, acc, true),
                            ));
                        }
                        _ => {}
                    }
                }
                None
            },
        );

        Ok(Box::pin(s))
    }
}
