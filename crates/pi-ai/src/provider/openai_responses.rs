//! OpenAI Responses API (`POST /v1/responses`) provider path (RFD 0019).
//!
//! Mirrors `openai.rs` but speaks the newer wire shape required by the
//! gpt-5 family and the o-series reasoning models. Public surface:
//!
//! * [`build_request_body`] — construct the JSON request body.
//! * [`messages_to_responses_input`] — pi-rs `Message` → Responses
//!   `input` items.
//! * [`tool_to_responses_tool`] — flatten our `ToolSpec` into the
//!   Responses tool shape (no nested `function:{}` wrapper).
//! * [`effort_block`] — pi-rs `ThinkingLevel` → `reasoning:{effort,
//!   summary}` block.
//! * [`stream_responses`] — drive the SSE event router; emits the
//!   provider-agnostic [`StreamEvent`] sequence.
//!
//! The `ApiKind` enum below is a *local* stub: the
//! `claude/responses-registry` branch will add the same enum on
//! `ModelInfo`. When that branch merges first the local stub is
//! superseded naturally; until then we route based on a model-id
//! heuristic in [`pick_api_kind`].

use eventsource_stream::Eventsource;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};

use crate::cost::UsageAcc;
use crate::message::{ContentBlock, FinishReason, Role, ThinkingLevel};
use crate::provider::GenerateRequest;
use crate::registry::ModelInfo;
use crate::stream::{StreamEvent, StreamEventKind};
use crate::tool::ToolSpec;
use crate::{AiError, Result};

use super::openai::OpenAiProvider;
use super::EventStream;

/// Stub of the registry-branch `ApiKind`. Gets shadowed by
/// `crate::registry::ApiKind` once `claude/responses-registry`
/// lands. Mirrors the RFD 0019 §1 shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKind {
    /// `POST /v1/chat/completions` — legacy default.
    ChatCompletions,
    /// `POST /v1/responses` — gpt-5.x and o-series.
    Responses,
}

/// Pick the API surface for a given model. Heuristic stand-in for the
/// `model.api_kind` field that lands on `claude/responses-registry`.
pub fn pick_api_kind(model: &ModelInfo) -> ApiKind {
    let id = model.id.as_str();
    // gpt-5 family + o-series reasoning models route to Responses.
    if id.starts_with("gpt-5")
        || id == "o1"
        || id.starts_with("o1-")
        || id == "o3"
        || id.starts_with("o3-")
        || id == "o4"
        || id.starts_with("o4-")
    {
        ApiKind::Responses
    } else {
        ApiKind::ChatCompletions
    }
}

// ---------------------------------------------------------------------------
// Request body construction
// ---------------------------------------------------------------------------

/// Convert pi-rs `Message`s into Responses `input` items (RFD 0019 §3).
///
/// Each pi-rs message can fan out to several input items because
/// Responses splits assistant text from `function_call` siblings.
pub fn messages_to_responses_input(msgs: &[crate::message::Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in msgs {
        match m.role {
            Role::System => {
                let text = collect_text(&m.content);
                if !text.is_empty() {
                    out.push(json!({
                        "role": "system",
                        "content": [{"type": "input_text", "text": text}],
                    }));
                }
            }
            Role::User | Role::Tool => {
                // User messages may carry text + image attachments + tool
                // results. Tool results become standalone
                // `function_call_output` items per the RFD; everything
                // else collapses into one user message.
                let mut user_content: Vec<Value> = Vec::new();
                for c in &m.content {
                    match c {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                user_content.push(json!({
                                    "type": "input_text",
                                    "text": text,
                                }));
                            }
                        }
                        ContentBlock::Attachment { attachment } => {
                            if let crate::message::AttachmentKind::Image { mime, base64 } =
                                &attachment.kind
                            {
                                let url = format!("data:{};base64,{}", mime, base64);
                                user_content.push(json!({
                                    "type": "input_image",
                                    "image_url": url,
                                }));
                            }
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            out.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content,
                            }));
                        }
                        // Thinking / ToolUse on a user message is nonsense;
                        // Responses doesn't accept them.
                        _ => {}
                    }
                }
                if !user_content.is_empty() {
                    out.push(json!({
                        "role": "user",
                        "content": user_content,
                    }));
                }
            }
            Role::Assistant => {
                let mut msg_content: Vec<Value> = Vec::new();
                for c in &m.content {
                    match c {
                        ContentBlock::Text { text } => {
                            if !text.is_empty() {
                                msg_content.push(json!({
                                    "type": "output_text",
                                    "text": text,
                                }));
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            // Flush any accumulated text first so item
                            // order matches the original conversation.
                            if !msg_content.is_empty() {
                                out.push(json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": std::mem::take(&mut msg_content),
                                }));
                            }
                            out.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": serde_json::to_string(input).unwrap_or_default(),
                            }));
                        }
                        // Reasoning / attachments / nested tool_results
                        // on an assistant message: drop. Encrypted
                        // reasoning round-trips are out of scope (RFD
                        // 0019 "Out of scope").
                        _ => {}
                    }
                }
                if !msg_content.is_empty() {
                    out.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": msg_content,
                    }));
                }
            }
        }
    }
    out
}

fn collect_text(content: &[ContentBlock]) -> String {
    let mut s = String::new();
    for c in content {
        if let ContentBlock::Text { text } = c {
            s.push_str(text);
        }
    }
    s
}

/// Flatten a pi-rs `ToolSpec` to the Responses tool shape: a flat
/// `{type, name, description, parameters, strict}` object — *no*
/// nested `function:{}` wrapper (which is the Chat Completions form).
pub fn tool_to_responses_tool(t: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": t.name,
        "description": t.description,
        "parameters": t.input_schema,
        "strict": false,
    })
}

/// Build the `reasoning:{effort, summary?}` block. Returns `None` for
/// `Off`. `Adaptive` (Anthropic-side concept) maps to
/// `effort:"high", summary:"auto"` for now — Responses has no native
/// adaptive switch (RFD 0019 open question).
///
/// Pi-rs's `ThinkingLevel` only has `Off|Low|Medium|High` today; we
/// accept any of those plus a sentinel "high+summary:auto" caller-side.
pub fn effort_block(t: ThinkingLevel) -> Option<Value> {
    match t {
        ThinkingLevel::Off => None,
        ThinkingLevel::Low => Some(json!({"effort": "low"})),
        ThinkingLevel::Medium => Some(json!({"effort": "medium"})),
        ThinkingLevel::High => Some(json!({"effort": "high", "summary": "auto"})),
        ThinkingLevel::XHigh => Some(json!({"effort": "xhigh", "summary": "auto"})),
    }
}

/// Construct the JSON body for `POST /v1/responses` (RFD 0019 §2).
pub fn build_request_body(req: &GenerateRequest, model: &ModelInfo) -> Value {
    let mut input = Vec::new();
    if let Some(sys) = &req.system {
        input.push(json!({
            "role": "system",
            "content": [{"type": "input_text", "text": sys}],
        }));
    }
    input.extend(messages_to_responses_input(&req.messages));

    let mut body = json!({
        "model": model.id,
        "input": input,
        "stream": true,
        "store": false,
    });
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = req.max_output_tokens {
        body["max_output_tokens"] = json!(m);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(tool_to_responses_tool).collect());
    }
    if model.supports_thinking {
        if let Some(reasoning) = effort_block(req.thinking) {
            body["reasoning"] = reasoning;
            // Encrypted reasoning blob — discarded for v1 (RFD 0019
            // out-of-scope) but requested so cache stays warm.
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
    }
    body
}

// ---------------------------------------------------------------------------
// SSE event router
// ---------------------------------------------------------------------------

/// Per-call_id state for an in-flight `function_call` item. Indexed by
/// the Responses `item_id` (the unique id assigned to the streamed
/// item, distinct from `call_id`).
#[derive(Default, Clone)]
struct ToolItem {
    call_id: String,
    name: String,
    args_buf: String,
}

/// State carried through the SSE event router across stream-yields.
struct RouterState {
    es: Box<
        dyn futures::Stream<
                Item = std::result::Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<reqwest::Error>,
                >,
            > + Send
            + Unpin,
    >,
    tools: std::collections::HashMap<String, ToolItem>,
    usage: UsageAcc,
    model: ModelInfo,
    done: bool,
    /// Pending `Finish` to emit after `Usage` on response.completed.
    pending_finish: Option<FinishReason>,
}

/// Map a Responses `response.status` to pi-rs's [`FinishReason`].
fn map_status(status: &str) -> FinishReason {
    match status {
        "completed" => FinishReason::Stop,
        "incomplete" => FinishReason::Length,
        "failed" | "cancelled" => FinishReason::Other, // RFD: Error → bubbles via Error event
        _ => FinishReason::Other,
    }
}

/// POST `/v1/responses` and stream the parsed events.
pub async fn stream_responses(
    provider: &OpenAiProvider,
    req: GenerateRequest,
    model: &ModelInfo,
) -> Result<EventStream> {
    let url = format!("{}/responses", provider.config.base_url);
    let auth_value = auth_header(provider)?;
    let body = build_request_body(&req, model);

    let resp = provider
        .client
        .post(&url)
        .header(provider.config.auth_header.as_str(), auth_value)
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
    let state = RouterState {
        es: Box::new(event_stream),
        tools: std::collections::HashMap::new(),
        usage: UsageAcc::default(),
        model: model.clone(),
        done: false,
        pending_finish: None,
    };

    let s = stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        // Emit a deferred Finish that follows the Usage event.
        if let Some(reason) = st.pending_finish.take() {
            st.done = true;
            return Some((Ok(StreamEvent::new(StreamEventKind::Finish { reason })), st));
        }
        loop {
            let next = st.es.next().await;
            let ev = match next {
                Some(Ok(ev)) => ev,
                Some(Err(e)) => {
                    st.done = true;
                    return Some((
                        Ok(StreamEvent::new(StreamEventKind::Error {
                            message: e.to_string(),
                        })),
                        st,
                    ));
                }
                None => {
                    return None;
                }
            };
            // Event-stream framing: data lines hold a JSON envelope whose
            // own `type` field is what we route on (the SSE `event:` field
            // is also set but we trust the body).
            if ev.data.is_empty() {
                continue;
            }
            let data: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let kind = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                // 1. session start — nothing to surface.
                "response.created" => continue,

                // 2. start a new output item (reasoning|message|function_call).
                "response.output_item.added" => {
                    let item = data.get("item").cloned().unwrap_or(Value::Null);
                    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match item_type {
                        "function_call" => {
                            let item_id = item
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let call_id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            st.tools.insert(
                                item_id,
                                ToolItem {
                                    call_id: call_id.clone(),
                                    name: name.clone(),
                                    args_buf: String::new(),
                                },
                            );
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::ToolCallStart {
                                    id: call_id,
                                    name,
                                })),
                                st,
                            ));
                        }
                        "message" => {
                            return Some((Ok(StreamEvent::new(StreamEventKind::MessageStart)), st));
                        }
                        // reasoning items have no pi-rs "start" event;
                        // ThinkingDelta is what surfaces.
                        _ => continue,
                    }
                }

                // 3. reasoning summary text delta.
                "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        if !delta.is_empty() {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::ThinkingDelta {
                                    text: delta.to_string(),
                                })),
                                st,
                            ));
                        }
                    }
                    continue;
                }

                // 4. assistant text delta.
                "response.output_text.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        if !delta.is_empty() {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::TextDelta {
                                    text: delta.to_string(),
                                })),
                                st,
                            ));
                        }
                    }
                    continue;
                }

                // 5. tool argument JSON delta.
                "response.function_call_arguments.delta" => {
                    let item_id = data.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                    let delta = data.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(t) = st.tools.get_mut(item_id) {
                        t.args_buf.push_str(delta);
                        let id = t.call_id.clone();
                        if !delta.is_empty() {
                            return Some((
                                Ok(StreamEvent::new(StreamEventKind::ToolInputDelta {
                                    id,
                                    partial_json: delta.to_string(),
                                })),
                                st,
                            ));
                        }
                    }
                    continue;
                }

                // 6. tool arguments finalised. Authoritative
                // `arguments` string lives on this event; prefer it
                // over the locally-accumulated buffer.
                "response.function_call_arguments.done" => {
                    let item_id = data.get("item_id").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(t) = st.tools.get_mut(item_id) {
                        if let Some(args) = data.get("arguments").and_then(|v| v.as_str()) {
                            t.args_buf = args.to_string();
                        }
                    }
                    continue;
                }

                // 7. an output item closes. Flush function_call as
                // ToolCallComplete; messages have no tail event.
                "response.output_item.done" => {
                    let item = data.get("item").cloned().unwrap_or(Value::Null);
                    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if item_type == "function_call" {
                        let item_id = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // Final form of the item carries the canonical
                        // `arguments` string; fall back to local buf.
                        let final_args = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let info = st.tools.remove(&item_id).unwrap_or_default();
                        let call_id = if info.call_id.is_empty() {
                            item.get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        } else {
                            info.call_id
                        };
                        let name = if info.name.is_empty() {
                            item.get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        } else {
                            info.name
                        };
                        let buf = final_args.unwrap_or(info.args_buf);
                        let input: Value = if buf.is_empty() {
                            Value::Object(Default::default())
                        } else {
                            serde_json::from_str(&buf).unwrap_or(Value::Null)
                        };
                        return Some((
                            Ok(StreamEvent::new(StreamEventKind::ToolCallComplete {
                                id: call_id,
                                name,
                                input,
                            })),
                            st,
                        ));
                    }
                    continue;
                }

                // 8. terminal envelope. Pull usage, queue Finish.
                "response.completed" => {
                    let response = data.get("response").cloned().unwrap_or(Value::Null);
                    if let Some(u) = response.get("usage") {
                        st.usage.input_tokens =
                            u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        st.usage.output_tokens =
                            u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        // cached_tokens lives under input_tokens_details
                        // (or input_tokens.cached_tokens on some
                        // builds); accept either.
                        let cached = u
                            .get("input_tokens_details")
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(|v| v.as_u64())
                            .or_else(|| {
                                u.get("input_tokens")
                                    .and_then(|v| v.get("cached_tokens"))
                                    .and_then(|v| v.as_u64())
                            })
                            .unwrap_or(0);
                        st.usage.cache_read_tok = cached;
                        // Reasoning tokens, when surfaced.
                        st.usage.reasoning_tok = u
                            .get("output_tokens_details")
                            .and_then(|d| d.get("reasoning_tokens"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                    let status = response
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("completed");
                    let reason = map_status(status);
                    let usage = st.usage.into_usage(&st.model);
                    st.pending_finish = Some(reason);
                    return Some((Ok(StreamEvent::new(StreamEventKind::Usage { usage })), st));
                }

                // 9. provider-side failure or generic SSE error frame.
                "response.failed" | "error" => {
                    let message = data
                        .get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(|v| v.as_str())
                        .or_else(|| {
                            data.get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|v| v.as_str())
                        })
                        .or_else(|| data.get("message").and_then(|v| v.as_str()))
                        .unwrap_or("response.failed")
                        .to_string();
                    st.done = true;
                    return Some((Ok(StreamEvent::new(StreamEventKind::Error { message })), st));
                }

                // The remaining 5 event types
                // (response.reasoning_summary_part.{added,done},
                //  response.content_part.added,
                //  response.refusal.delta,
                //  …)
                // carry no information pi-rs surfaces today. Ignore.
                _ => continue,
            }
        }
    });

    Ok(Box::pin(s))
}

fn auth_header(provider: &OpenAiProvider) -> Result<String> {
    let token = match &provider.auth {
        crate::auth::AuthMethod::ApiKey { value } => value.clone(),
        crate::auth::AuthMethod::OAuth { access_token, .. } => access_token.clone(),
        crate::auth::AuthMethod::None => {
            return Err(AiError::MissingAuth(provider.config.name.clone()))
        }
    };
    Ok(provider.config.auth_format.replace("{token}", &token))
}
