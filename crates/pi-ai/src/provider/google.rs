use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::AuthMethod;
use crate::cost::UsageAcc;
use crate::message::{ContentBlock, FinishReason, Role};
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
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => parts.push(json!({
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

    async fn discover_models(&self) -> Result<Vec<ModelInfo>> {
        super::discover::google(&self.client, &self.config, &self.auth).await
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
        let model_owned = model.clone();
        let s = stream::unfold(
            (
                event_stream,
                UsageAcc::default(),
                None::<FinishReason>,
                model_owned,
            ),
            move |(mut es, mut usage_running, mut pending_finish, model_owned)| async move {
                // If we deferred a Finish from the previous chunk (after emitting
                // Usage), surface it now.
                if let Some(reason) = pending_finish.take() {
                    return Some((
                        Ok(StreamEvent::new(StreamEventKind::Finish { reason })),
                        (es, usage_running, None, model_owned),
                    ));
                }
                while let Some(item) = es.next().await {
                    let ev = match item {
                        Ok(ev) => ev,
                        Err(e) => {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::Error {
                                    message: e.to_string(),
                                })),
                                (es, usage_running, None, model_owned),
                            ));
                        }
                    };
                    let data: Value = match serde_json::from_str(&ev.data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    // Update cumulative usage on every chunk that carries it.
                    let has_meta = data.get("usageMetadata").is_some();
                    if let Some(meta) = data.get("usageMetadata") {
                        usage_running.input_tokens = meta
                            .get("promptTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(usage_running.input_tokens);
                        usage_running.output_tokens = meta
                            .get("candidatesTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(usage_running.output_tokens);
                        usage_running.cache_read_tok = meta
                            .get("cachedContentTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(usage_running.cache_read_tok);
                        usage_running.reasoning_tok = meta
                            .get("thoughtsTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(usage_running.reasoning_tok);
                    }
                    let has_candidates = data
                        .get("candidates")
                        .and_then(|v| v.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    // Standalone-usageMetadata chunk (no candidates):
                    // emit Usage now. Real Gemini puts cumulative metadata
                    // on every chunk including the terminal one, but some
                    // wire variants ship a trailing metadata-only chunk.
                    if has_meta && !has_candidates {
                        let usage = usage_running.into_usage(&model_owned);
                        return Some((
                            Ok(StreamEvent::new(StreamEventKind::Usage { usage })),
                            (es, usage_running, pending_finish, model_owned),
                        ));
                    }
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
                                                (es, usage_running, pending_finish, model_owned),
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
                                            (es, usage_running, pending_finish, model_owned),
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
                                // Terminal chunk: emit one Usage event built from
                                // the cumulative `usage_running`, then defer the
                                // Finish to the next poll.
                                let usage = usage_running.into_usage(&model_owned);
                                return Some((
                                    Ok(StreamEvent::new(StreamEventKind::Usage { usage })),
                                    (es, usage_running, Some(r), model_owned),
                                ));
                            }
                        }
                    }
                }
                None
            },
        );
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
