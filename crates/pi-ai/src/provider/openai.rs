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

/// OpenAI Chat Completions provider (also serves as parent class for
/// OpenAI-compatible providers via OpenAiCompatProvider).
pub struct OpenAiProvider {
    pub config: ProviderConfig,
    pub auth: AuthMethod,
    pub client: Client,
}

/// OpenAI-compatible provider — same wire format, different base URL.
pub struct OpenAiCompatProvider(pub OpenAiProvider);

impl OpenAiProvider {
    pub fn new(config: ProviderConfig, auth: AuthMethod) -> Self {
        Self {
            config,
            auth,
            client: Client::new(),
        }
    }

    fn auth_header(&self) -> Result<String> {
        let token = match &self.auth {
            AuthMethod::ApiKey { value } => value.clone(),
            AuthMethod::OAuth { access_token, .. } => access_token.clone(),
            AuthMethod::None => return Err(AiError::MissingAuth(self.config.name.clone())),
        };
        Ok(self.config.auth_format.replace("{token}", &token))
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn message_to_openai(m: &crate::message::Message) -> Vec<Value> {
    // OpenAI splits a single Anthropic-style assistant message with multiple
    // tool_use blocks into one assistant message + N tool messages.
    let mut out = Vec::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_calls = Vec::new();
    let mut tool_results: Vec<(String, String, bool)> = Vec::new();
    for c in &m.content {
        match c {
            ContentBlock::Text { text: t } => text.push_str(t),
            ContentBlock::Thinking { text: t, .. } => thinking.push_str(t),
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input).unwrap_or_default(),
                    }
                }));
            }
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                tool_results.push((tool_use_id.clone(), content.clone(), *is_error));
            }
            ContentBlock::Attachment { attachment } => {
                if let crate::message::AttachmentKind::Image { mime, base64 } = &attachment.kind {
                    let url = format!("data:{};base64,{}", mime, base64);
                    text.push_str(&format!("\n[image:{}]\n", url));
                }
            }
        }
    }
    if !text.is_empty() || !tool_calls.is_empty() || !thinking.is_empty() {
        let mut content_str = String::new();
        if !thinking.is_empty() {
            content_str.push_str("<thinking>");
            content_str.push_str(&thinking);
            content_str.push_str("</thinking>\n");
        }
        content_str.push_str(&text);
        let mut msg = json!({"role": role_str(m.role), "content": content_str});
        if !tool_calls.is_empty() {
            msg["tool_calls"] = Value::Array(tool_calls);
            if matches!(m.role, Role::Assistant) && content_str.is_empty() {
                msg["content"] = Value::Null;
            }
        }
        out.push(msg);
    }
    for (id, content, _is_error) in tool_results {
        out.push(json!({
            "role": "tool",
            "tool_call_id": id,
            "content": content,
        }));
    }
    out
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn auth(&self) -> &AuthMethod {
        &self.auth
    }

    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let auth_value = self.auth_header()?;

        let mut messages: Vec<Value> = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        for m in &req.messages {
            messages.extend(message_to_openai(m));
        }

        let mut body = json!({
            "model": model.id,
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
            .header(self.config.auth_header.as_str(), auth_value)
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

        let s = stream::unfold((event_stream, acc, false), move |(mut es, mut acc, mut done)| async move {
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
                        ))
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
                        input_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        output_tokens: usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
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
                if let Some(choice) = data.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) {
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
                    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                        if !reasoning.is_empty() {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::ThinkingDelta {
                                    text: reasoning.to_string(),
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
                                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                    entry.2.push_str(args);
                                    if !entry.0.is_empty() {
                                        return Some((
                                            Ok(StreamEvent::new(StreamEventKind::ToolInputDelta {
                                                id: entry.0.clone(),
                                                partial_json: args.to_string(),
                                            })),
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
                        // emit any complete tool calls before finishing
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
        });

        Ok(Box::pin(s))
    }
}

impl OpenAiCompatProvider {
    pub fn new(config: ProviderConfig, auth: AuthMethod) -> Self {
        Self(OpenAiProvider::new(config, auth))
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn config(&self) -> &ProviderConfig {
        self.0.config()
    }
    fn auth(&self) -> &AuthMethod {
        self.0.auth()
    }
    async fn stream(&self, req: GenerateRequest, model: &ModelInfo) -> Result<EventStream> {
        self.0.stream(req, model).await
    }
}
