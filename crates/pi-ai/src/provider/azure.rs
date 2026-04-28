use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::AuthMethod;
use crate::message::{FinishReason, Usage};
use crate::registry::{ModelInfo, ProviderConfig};
use crate::stream::{StreamEvent, StreamEventKind};
use crate::{AiError, GenerateRequest, Result};

use super::{EventStream, Provider};

/// Azure OpenAI provider.
///
/// Uses the OpenAI Chat Completions wire format but:
/// - path: `/openai/deployments/{deployment_id}/chat/completions?api-version={version}`
/// - auth header: `api-key: <value>` (not `Authorization: Bearer`)
///
/// The `model.id` field is used as the deployment name.
pub struct AzureOpenAiProvider {
    pub config: ProviderConfig,
    pub auth: AuthMethod,
    pub client: Client,
    pub api_version: String,
}

impl AzureOpenAiProvider {
    /// Create a new provider. `api_version` defaults to `"2024-10-21"`.
    pub fn new(config: ProviderConfig, auth: AuthMethod) -> Self {
        Self {
            config,
            auth,
            client: Client::new(),
            api_version: "2024-10-21".to_string(),
        }
    }

    pub fn with_api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
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
impl Provider for AzureOpenAiProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn auth(&self) -> &AuthMethod {
        &self.auth
    }

    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream> {
        let token = self.auth_token()?.to_string();

        // Azure path: /openai/deployments/{deployment}/chat/completions?api-version=...
        let url = format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            self.config.base_url, model.id, self.api_version
        );

        let mut messages: Vec<Value> = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        for m in &req.messages {
            messages.extend(super::openai::message_to_openai(m));
        }

        let mut body = json!({
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_output_tokens {
            body["max_tokens"] = json!(m);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.input_schema,
                            }
                        })
                    })
                    .collect(),
            );
        }

        let resp = self
            .client
            .post(&url)
            .header("api-key", token)
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

        // Tool call accumulator: index -> (id, name, json_buf)
        let acc: std::collections::HashMap<u64, (String, String, String)> =
            std::collections::HashMap::new();

        let s = stream::unfold(
            (event_stream, acc, false),
            move |(mut es, mut acc, mut done)| async move {
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
                    if ev.data == "[DONE]" {
                        // flush any pending tool calls
                        let pending: Vec<(String, String, String)> =
                            acc.drain().map(|(_, v)| v).collect();
                        if let Some((id, name, buf)) = pending.into_iter().next() {
                            let input = serde_json::from_str(&buf).unwrap_or(Value::Null);
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::ToolCallComplete {
                                    id,
                                    name,
                                    input,
                                })),
                                (es, acc, true),
                            ));
                        }
                        done = true;
                        let _ = done;
                        return Some((
                            Ok(StreamEvent::new(StreamEventKind::Finish {
                                reason: FinishReason::Stop,
                            })),
                            (es, acc, true),
                        ));
                    }
                    let data: Value = match serde_json::from_str(&ev.data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Some(usage) = data.get("usage") {
                        let u = Usage {
                            input_tokens: usage
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            output_tokens: usage
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            reasoning_tokens: usage
                                .get("completion_tokens_details")
                                .and_then(|d| d.get("reasoning_tokens"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            ..Default::default()
                        };
                        return Some((
                            Ok(StreamEvent::new(StreamEventKind::Usage { usage: u })),
                            (es, acc, false),
                        ));
                    }
                    if let Some(choice) = data
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                    {
                        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
                        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::TextDelta {
                                        text: text.to_string(),
                                    })),
                                    (es, acc, false),
                                ));
                            }
                        }
                        if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for call in calls {
                                let idx = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                let entry = acc.entry(idx).or_insert_with(|| {
                                    (String::new(), String::new(), String::new())
                                });
                                if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                                    if !id.is_empty() {
                                        entry.0 = id.to_string();
                                    }
                                }
                                if let Some(func) = call.get("function") {
                                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                        if !name.is_empty() {
                                            entry.1 = name.to_string();
                                        }
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(|v| v.as_str())
                                    {
                                        entry.2.push_str(args);
                                        if !entry.0.is_empty() {
                                            return Some((
                                                Ok(StreamEvent::new(
                                                    StreamEventKind::ToolInputDelta {
                                                        id: entry.0.clone(),
                                                        partial_json: args.to_string(),
                                                    },
                                                )),
                                                (es, acc, false),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                            let r = match reason {
                                "tool_calls" | "function_call" => FinishReason::ToolUse,
                                "stop" => FinishReason::Stop,
                                "length" => FinishReason::Length,
                                "content_filter" => FinishReason::Refusal,
                                _ => FinishReason::Other,
                            };
                            if !acc.is_empty() {
                                let (idx, (id, name, buf)) =
                                    acc.iter().next().map(|(k, v)| (*k, v.clone())).unwrap();
                                acc.remove(&idx);
                                let input = serde_json::from_str(&buf).unwrap_or(Value::Null);
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::ToolCallComplete {
                                        id,
                                        name,
                                        input,
                                    })),
                                    (es, acc, false),
                                ));
                            }
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::Finish { reason: r })),
                                (es, acc, false),
                            ));
                        }
                    }
                }
                None
            },
        );

        Ok(Box::pin(s))
    }
}
