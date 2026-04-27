use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::AuthMethod;
use crate::message::{ContentBlock, FinishReason, Role, Usage};
use crate::registry::{ModelInfo, ProviderConfig};
use crate::stream::{StreamEvent, StreamEventKind};
use crate::{AiError, GenerateRequest, Result};

use super::{EventStream, Provider};

/// Google Generative AI provider (Gemini API).
///
/// Wire format: POST to
/// `{base_url}/v1beta/models/{model}:streamGenerateContent?alt=sse&key={api_key}`
/// with `contents`, `systemInstruction`, `generationConfig`, optional `tools`.
pub struct GoogleProvider {
    pub config: ProviderConfig,
    pub auth: AuthMethod,
    pub client: Client,
}

impl GoogleProvider {
    pub fn new(config: ProviderConfig, auth: AuthMethod) -> Self {
        Self {
            config,
            auth,
            client: Client::new(),
        }
    }

    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    fn token(&self) -> Result<String> {
        match &self.auth {
            AuthMethod::ApiKey { value } => Ok(value.clone()),
            AuthMethod::OAuth { access_token, .. } => Ok(access_token.clone()),
            AuthMethod::None => Err(AiError::MissingAuth(self.config.name.clone())),
        }
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User | Role::Tool | Role::System => "user",
        Role::Assistant => "model",
    }
}

pub fn message_to_google_parts(blocks: &[ContentBlock]) -> Vec<Value> {
    let mut parts = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => parts.push(json!({"text": text})),
            ContentBlock::Thinking { text, .. } => {
                parts.push(json!({"text": format!("<thinking>{text}</thinking>")}))
            }
            ContentBlock::ToolUse { name, input, .. } => parts.push(json!({
                "functionCall": {"name": name, "args": input}
            })),
            ContentBlock::ToolResult { tool_use_id, content, is_error } => parts.push(json!({
                "functionResponse": {
                    "name": tool_use_id,
                    "response": {"content": content, "is_error": is_error},
                }
            })),
            ContentBlock::Attachment { attachment } => match &attachment.kind {
                crate::message::AttachmentKind::Image { mime, base64 } => parts.push(json!({
                    "inline_data": {"mime_type": mime, "data": base64}
                })),
                crate::message::AttachmentKind::File { mime, base64, .. } => parts.push(json!({
                    "inline_data": {"mime_type": mime, "data": base64}
                })),
            },
        }
    }
    parts
}

#[async_trait]
impl Provider for GoogleProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }
    fn auth(&self) -> &AuthMethod {
        &self.auth
    }

    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream> {
        let token = self.token()?;
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.config.base_url, model.id, token
        );

        let contents: Vec<Value> = req
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                json!({
                    "role": role_str(m.role),
                    "parts": message_to_google_parts(&m.content),
                })
            })
            .collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": req.max_output_tokens.unwrap_or(model.max_output_tokens),
            }
        });
        if let Some(t) = req.temperature {
            body["generationConfig"]["temperature"] = json!(t);
        }
        if let Some(sys) = &req.system {
            body["systemInstruction"] = json!({"parts": [{"text": sys}]});
        }
        if !req.tools.is_empty() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = json!([{"functionDeclarations": decls}]);
        }

        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AiError::Provider { status, body });
        }

        let event_stream = resp.bytes_stream().eventsource();
        let s = stream::unfold(event_stream, move |mut es| async move {
            while let Some(item) = es.next().await {
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        return Some((
                            Ok(StreamEvent::new(StreamEventKind::Error {
                                message: e.to_string(),
                            })),
                            es,
                        ));
                    }
                };
                let data: Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(candidates) = data.get("candidates").and_then(|v| v.as_array()) {
                    if let Some(c) = candidates.first() {
                        if let Some(parts) = c
                            .get("content")
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        return Some((
                                            Ok(StreamEvent::new(StreamEventKind::TextDelta {
                                                text: text.to_string(),
                                            })),
                                            es,
                                        ));
                                    }
                                }
                                if let Some(fc) = part.get("functionCall") {
                                    let name = fc
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let args = fc.get("args").cloned().unwrap_or(Value::Null);
                                    let id = format!("call_{}", call_id_seed(&name));
                                    return Some((
                                        Ok(StreamEvent::new(
                                            StreamEventKind::ToolCallComplete {
                                                id,
                                                name,
                                                input: args,
                                            },
                                        )),
                                        es,
                                    ));
                                }
                            }
                        }
                        if let Some(reason) = c.get("finishReason").and_then(|v| v.as_str()) {
                            let r = match reason {
                                "STOP" => FinishReason::Stop,
                                "MAX_TOKENS" => FinishReason::Length,
                                "SAFETY" | "RECITATION" => FinishReason::Refusal,
                                "TOOL_USE" | "TOOL_CALL" => FinishReason::ToolUse,
                                _ => FinishReason::Other,
                            };
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::Finish { reason: r })),
                                es,
                            ));
                        }
                    }
                }
                if let Some(usage_meta) = data.get("usageMetadata") {
                    let usage = Usage {
                        input_tokens: usage_meta
                            .get("promptTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        output_tokens: usage_meta
                            .get("candidatesTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        ..Default::default()
                    };
                    return Some((
                        Ok(StreamEvent::new(StreamEventKind::Usage { usage })),
                        es,
                    ));
                }
            }
            None
        });
        Ok(Box::pin(s))
    }
}

fn call_id_seed(seed: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
        .hash(&mut h);
    format!("{:x}", h.finish())
}
