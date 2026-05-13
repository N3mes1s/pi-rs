use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::AuthMethod;
use crate::cost::UsageAcc;
use crate::message::{Role, ThinkingLevel};
use crate::registry::{ModelInfo, ProviderConfig};
use crate::stream::{StreamEvent, StreamEventKind};
use crate::{AiError, GenerateRequest, Result};

use super::anthropic_stream::{handle_anthropic_event, ToolAcc};
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
            client: crate::http::streaming_client_or_default(),
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
                    // Bedrock has no xhigh tier; share the High budget.
                    ThinkingLevel::High | ThinkingLevel::XHigh => 32_000,
                    ThinkingLevel::Off => unreachable!(),
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
        let tool_inputs: ToolAcc = std::collections::HashMap::new();

        let model_owned = model.clone();
        let s = stream::unfold(
            (event_stream, tool_inputs, false, UsageAcc::default()),
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
                                            "bedrock: SSE stream idle for {}s — \
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

        Ok(Box::pin(s))
    }
}
