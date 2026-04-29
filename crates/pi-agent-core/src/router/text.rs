//! Text-level helpers shared across the router: prompt assembly,
//! cosine similarity, hashed embeddings, tokenisation/stemming, and
//! the `parse_thinking` / `parse_tale_ep_budget` / `resolve_force`
//! parsers. Pure functions — no I/O, no global state beyond a salt
//! constant.

use crate::router::{ForceOverride, RoutingDecision, ToolSpec};
use pi_ai::{Message, ThinkingLevel};
use std::sync::OnceLock;

pub(super) fn router_input(prompt: &str, history: &[Message], tools: &[ToolSpec]) -> String {
    let mut out = prompt.trim().to_string();
    if !history.is_empty() {
        out.push_str("\n\nconversation_context:\n");
        for msg in history.iter().rev().take(4).rev() {
            out.push_str(&message_text(msg));
            out.push('\n');
        }
    }
    if !tools.is_empty() {
        out.push_str("\navailable_tools:");
        for tool in tools {
            out.push(' ');
            out.push_str(&tool.name);
        }
    }
    out
}

fn message_text(message: &Message) -> String {
    let mut parts = Vec::new();
    for block in &message.content {
        match block {
            pi_ai::ContentBlock::Text { text } | pi_ai::ContentBlock::Thinking { text, .. } => {
                parts.push(text.clone())
            }
            pi_ai::ContentBlock::ToolUse { name, .. } => parts.push(format!("tool:{name}")),
            pi_ai::ContentBlock::ToolResult { content, .. } => parts.push(content.clone()),
            _ => {}
        }
    }
    parts.join(" ")
}

pub(super) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

pub(super) fn l2_normalize(values: Vec<f32>) -> Vec<f32> {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 {
        values
    } else {
        values.into_iter().map(|v| v / norm).collect()
    }
}

pub(super) fn hashed_embedding(text: &str) -> Vec<f32> {
    const DIMS: usize = 256;
    static SALT: OnceLock<u64> = OnceLock::new();
    let salt = *SALT.get_or_init(|| 0x9E37_79B9_7F4A_7C15);
    let mut out = vec![0.0f32; DIMS];
    for token in tokenize(text) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        salt.hash(&mut h);
        token.hash(&mut h);
        let hash = h.finish();
        let idx = (hash as usize) % DIMS;
        let sign = if (hash >> 63) == 0 { 1.0 } else { -1.0 };
        out[idx] += sign;
        let secondary = ((hash >> 32) as usize) % DIMS;
        out[secondary] += sign * 0.5;
    }
    out
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(stem_token)
        .filter(|s| !s.is_empty())
        .collect()
}

fn stem_token(token: &str) -> String {
    let mut t = token.to_string();
    for suffix in ["ing", "ed", "es", "s"] {
        if t.len() > suffix.len() + 2 && t.ends_with(suffix) {
            t.truncate(t.len() - suffix.len());
            break;
        }
    }
    t
}

pub(super) fn parse_thinking(value: &str) -> ThinkingLevel {
    match value {
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::XHigh,
        _ => ThinkingLevel::Off,
    }
}

/// Parse a TALE-EP `<budget>N</budget>` tag out of `prompt` and return
/// the numeric token budget. Telemetry-only — the runtime emits this on
/// the `hard` route's `RoutingDecision` session entry but never gates
/// the dispatch on it. Tag matching is forgiving: leading/trailing
/// whitespace inside the tag is tolerated; the first valid tag wins.
pub fn parse_tale_ep_budget(prompt: &str) -> Option<u64> {
    const OPEN: &str = "<budget>";
    const CLOSE: &str = "</budget>";
    let mut cursor = 0;
    while let Some(rel_open) = prompt[cursor..].find(OPEN) {
        let open = cursor + rel_open + OPEN.len();
        let Some(rel_close) = prompt[open..].find(CLOSE) else {
            return None;
        };
        let close = open + rel_close;
        let inner = prompt[open..close].trim();
        if let Ok(n) = inner.parse::<u64>() {
            return Some(n);
        }
        cursor = close + CLOSE.len();
    }
    None
}

pub(super) fn resolve_force(force: &ForceOverride) -> RoutingDecision {
    match force {
        ForceOverride::CliFlag {
            provider,
            model,
            thinking,
        } => {
            let (provider_name, model_name) = match provider {
                Some(p) => (p.clone(), model.clone()),
                None => model
                    .split_once('/')
                    .map(|(p, m)| (p.to_string(), m.to_string()))
                    .unwrap_or(("anthropic".into(), model.clone())),
            };
            RoutingDecision {
                route_id: "forced".into(),
                provider: provider_name,
                model: model_name,
                thinking: thinking.unwrap_or(ThinkingLevel::Off),
            }
        }
    }
}
