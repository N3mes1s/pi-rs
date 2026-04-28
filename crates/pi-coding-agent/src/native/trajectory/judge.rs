//! Agentic outcome judge — G2.
//!
//! Reads the full session context (user request, agent's final reply,
//! extracted features, action digest) and asks a smol model whether the
//! agent achieved the user's goal. Returns a structured verdict that maps
//! to [`SessionEntryKind::Outcome`] with `source = OutcomeSource::LlmJudge`.
//!
//! Why agentic instead of deterministic: tests-pass ≠ task solved;
//! tests-fail ≠ task failed when the user asked "investigate". The
//! verdict has to read the actual trajectory.
//!
//! The judge is best-effort. On any error (timeout, bad JSON, no auth) it
//! falls back to deriving a heuristic-only `Outcome` from features alone,
//! tagged `OutcomeSource::Heuristic`. Trajectory recording itself never
//! fails.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use pi_agent_core::{OutcomeSource, SessionEntry, SessionEntryKind};
use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider,
    BedrockAnthropicProvider, GenerateRequest, GoogleProvider, Message, ModelInfo,
    ModelRegistry, OpenAiCompatProvider, OpenAiProvider, Provider, ProviderConfig,
    ProviderKind, ThinkingLevel,
};

use super::features::{extract, Termination, TrajectoryFeatures};

const SYSTEM_PROMPT: &str = "\
You are an outcome auditor for an autonomous coding agent. Given the \
user's task, the agent's final response, structured features extracted \
from the session, and a digest of the agent's actions, decide whether \
the agent achieved the user's goal.

Definitions:
- success: did the agent meaningfully accomplish what the user asked? Boolean.
- score:   confidence in your verdict, 0.0 (uncertain) to 1.0 (certain).
- reason:  one short paragraph explaining the decision.
- salient_wins:     what worked, 0-3 short bullets.
- salient_failures: what didn't, 0-3 short bullets.

Rules:
- Tests-pass alone is NOT success — verify the agent solved the user's actual request.
- Tests-fail alone is NOT failure — the user may have asked to investigate or explain.
- An agent stuck in a loop (repeatedly reading the same file without progress) is a failure.
- An agent that completed the task without running tests is fine if the task didn't require them.
- A final reply that says \"I cannot do this\" or punts back to the user, on a task the user \
  clearly wanted done, is a failure.
- A correct answer that quotes content from a `<context_loaded>` file is not a fabrication. \
  Tool calls are evidence of work, not a prerequisite for success. If the agent's reply is \
  grounded in a context file, score it on whether it answered the user's question — not on \
  whether it re-fetched the file.

Reply with a SINGLE JSON object on one line and nothing else:
{\"success\": true|false, \"score\": 0.0-1.0, \"reason\": \"...\", \"salient_wins\": [\"...\"], \"salient_failures\": [\"...\"]}";

const JUDGE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub thinking: ThinkingLevel,
    #[serde(default = "default_max_tokens")]
    pub max_output_tokens: u32,
}

fn default_max_tokens() -> u32 {
    512
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            thinking: ThinkingLevel::Off,
            max_output_tokens: 512,
        }
    }
}

/// Parsed verdict from the smol model. The full struct is logged in
/// `Outcome.notes` as JSON for the evolver's benefit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JudgeVerdict {
    pub success: bool,
    pub score: f32,
    pub reason: String,
    #[serde(default)]
    pub salient_wins: Vec<String>,
    #[serde(default)]
    pub salient_failures: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("unknown judge model: {0}")]
    UnknownModel(String),
    #[error("missing credentials for judge provider {0}")]
    MissingAuth(String),
    #[error("judge model returned non-JSON or wrong shape: {0}")]
    BadResponse(String),
    #[error("judge call timed out after {}s", JUDGE_TIMEOUT.as_secs())]
    Timeout,
    #[error("provider error: {0}")]
    Provider(String),
}

#[derive(Clone)]
pub struct Judge {
    config: JudgeConfig,
    provider: std::sync::Arc<dyn Provider>,
    model_info: ModelInfo,
}

impl Judge {
    pub fn build(
        registry: &ModelRegistry,
        auth: &AuthStorage,
        config: JudgeConfig,
    ) -> Result<Self, JudgeError> {
        let target = format!("{}/{}", config.provider, config.model);
        let (provider_cfg, model_info) = registry
            .resolve(&target)
            .or_else(|| registry.resolve(&config.model))
            .ok_or_else(|| JudgeError::UnknownModel(target.clone()))?;
        let auth_method = auth
            .get(&provider_cfg.name)
            .ok_or_else(|| JudgeError::MissingAuth(provider_cfg.name.clone()))?;
        let provider = build_provider(provider_cfg.clone(), auth_method);
        Ok(Self {
            config,
            provider: provider.into(),
            model_info: model_info.clone(),
        })
    }

    /// Score a session branch. Returns the verdict on success; on any
    /// error returns it so the caller can fall back to features-only.
    pub async fn judge(&self, branch: &[SessionEntry]) -> Result<JudgeVerdict, JudgeError> {
        let features = extract(branch);
        let user_text = build_user_message(branch, &features);

        let req = GenerateRequest {
            model: self.model_info.id.clone(),
            system: Some(SYSTEM_PROMPT.into()),
            messages: vec![Message::user_text(user_text)],
            tools: Vec::new(),
            thinking: self.config.thinking,
            temperature: Some(0.0),
            max_output_tokens: Some(self.config.max_output_tokens),
            extras: serde_json::Value::Null,
        };

        let provider = self.provider.clone();
        let model = self.model_info.clone();
        let resp = match tokio::time::timeout(JUDGE_TIMEOUT, provider.generate(req, &model)).await
        {
            Err(_) => return Err(JudgeError::Timeout),
            Ok(Err(e)) => return Err(JudgeError::Provider(e.to_string())),
            Ok(Ok(r)) => r,
        };

        parse_verdict(&resp.message.text())
    }
}

/// Top-level entry: judge the session, falling back to features-only when
/// the model is unavailable. Always returns an `Outcome` ready to append.
///
/// `judge` may be `None` (no API key, not configured); in that case we
/// produce a heuristic-only outcome.
pub async fn judge_session(
    branch: &[SessionEntry],
    judge: Option<&Judge>,
) -> Option<SessionEntryKind> {
    let features = extract(branch);
    if branch.iter().all(|e| !matches!(e.kind, SessionEntryKind::User { .. })) {
        return None;
    }

    if let Some(j) = judge {
        match j.judge(branch).await {
            Ok(verdict) => {
                return Some(SessionEntryKind::Outcome {
                    success: verdict.success,
                    source: OutcomeSource::LlmJudge,
                    score: Some(verdict.score.clamp(0.0, 1.0)),
                    notes: serde_json::to_string(&verdict).ok(),
                });
            }
            Err(_) => {} // fall through to features-only
        }
    }

    features_only_outcome(&features)
}

/// Best-effort verdict from features alone — used when the judge can't
/// run (no auth, timeout, parse failure). Lower confidence than the
/// agentic judge; meant as a safety net so trajectory recording doesn't
/// silently drop the session.
pub fn features_only_outcome(features: &TrajectoryFeatures) -> Option<SessionEntryKind> {
    let mut weight: f32 = 0.0;
    let mut signals = 0;
    let mut notes_parts = Vec::new();

    if let Some(last) = features.test_runs.last() {
        weight += if last.exit == 0 { 1.0 } else { -1.0 };
        signals += 1;
        notes_parts.push(format!("tests:{}={}", last.command, last.exit));
    }
    if let Some(last) = features.compile_runs.last() {
        weight += if last.exit == 0 { 0.5 } else { -0.5 };
        signals += 1;
        notes_parts.push(format!("compile:{}={}", last.command, last.exit));
    }
    if !features.repeated_reads.is_empty() {
        weight -= 0.4;
        signals += 1;
        notes_parts.push(format!("loop:{}", features.repeated_reads.len()));
    }
    let unrecovered = features.edit_errors.iter().filter(|e| !e.recovered).count();
    if unrecovered > 0 {
        weight -= 0.4;
        signals += 1;
        notes_parts.push(format!("edit_err:{unrecovered}"));
    }
    if matches!(features.last_termination, Termination::Error) {
        weight -= 0.3;
        signals += 1;
        notes_parts.push("term:error".into());
    }
    if signals == 0 {
        return None;
    }
    let mean = weight / signals as f32;
    let score = ((mean + 1.0) / 2.0).clamp(0.0, 1.0);
    Some(SessionEntryKind::Outcome {
        success: score > 0.5,
        source: OutcomeSource::Heuristic,
        score: Some(score),
        notes: Some(notes_parts.join("; ")),
    })
}

// ─── prompt assembly ────────────────────────────────────────────────────

pub fn build_user_message(branch: &[SessionEntry], features: &TrajectoryFeatures) -> String {
    let user_request = first_user_text(branch).unwrap_or_else(|| "(no user message)".into());
    let final_reply = last_assistant_text(branch).unwrap_or_else(|| "(no assistant reply)".into());
    let context_loaded = collect_context_loads(branch);
    let features_json =
        serde_json::to_string_pretty(features).unwrap_or_else(|_| "{}".into());
    let digest = action_digest(branch);

    format!(
        "<user_request>\n{}\n</user_request>\n\n\
         <context_loaded>\n{}\n</context_loaded>\n\n\
         <assistant_final_reply>\n{}\n</assistant_final_reply>\n\n\
         <features>\n{}\n</features>\n\n\
         <action_digest>\n{}\n</action_digest>",
        truncate(&user_request, 4000),
        context_loaded,
        truncate(&final_reply, 4000),
        features_json,
        digest,
    )
}

/// Collect any `ContextLoad` entries from the branch into a one-line-
/// per-file bullet list. Tells the judge which files (AGENTS.md,
/// CLAUDE.md, …) were already in the agent's system prompt — so a reply
/// that quotes them isn't mis-scored as a fabrication.
fn collect_context_loads(branch: &[SessionEntry]) -> String {
    let entries: Vec<String> = branch
        .iter()
        .filter_map(|e| match &e.kind {
            SessionEntryKind::ContextLoad { source, bytes, tokens } => Some(format!(
                "- {} ({} bytes, ~{} tokens)",
                source,
                bytes,
                tokens.unwrap_or(0),
            )),
            _ => None,
        })
        .collect();
    if entries.is_empty() {
        "(no context files were loaded into the system prompt)".into()
    } else {
        entries.join("\n")
    }
}

fn first_user_text(branch: &[SessionEntry]) -> Option<String> {
    branch.iter().find_map(|e| match &e.kind {
        SessionEntryKind::User { message } => Some(message.text()),
        _ => None,
    })
}

fn last_assistant_text(branch: &[SessionEntry]) -> Option<String> {
    branch.iter().rev().find_map(|e| match &e.kind {
        SessionEntryKind::Assistant { message } => Some(message.text()),
        _ => None,
    })
}

/// One line per tool call: `name(short_input) -> short_output_or_error`.
/// Capped at 30 entries (head + tail).
fn action_digest(branch: &[SessionEntry]) -> String {
    let mut lines = Vec::new();
    let mut calls = std::collections::HashMap::new();
    for e in branch {
        if let SessionEntryKind::ToolCall { call } = &e.kind {
            calls.insert(call.id.clone(), call.clone());
        }
    }
    for e in branch {
        if let SessionEntryKind::ToolResult { result } = &e.kind {
            let Some(call) = calls.get(&result.tool_use_id) else {
                continue;
            };
            let input_summary = match call.name.as_str() {
                "bash" => call
                    .input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|s| truncate(s, 80))
                    .unwrap_or_default(),
                "read" | "edit" | "write" => call
                    .input
                    .get("file_path")
                    .or_else(|| call.input.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                _ => truncate(&call.input.to_string(), 60),
            };
            let outcome = if result.is_error { "ERR" } else { "ok" };
            let tail = if result.is_error {
                format!(" :: {}", truncate(&result.model_output, 100))
            } else {
                String::new()
            };
            lines.push(format!(
                "{}({}) -> {}{}",
                call.name, input_summary, outcome, tail
            ));
        }
    }
    if lines.len() <= 30 {
        return lines.join("\n");
    }
    let head: Vec<_> = lines.iter().take(15).cloned().collect();
    let tail: Vec<_> = lines.iter().rev().take(15).rev().cloned().collect();
    format!(
        "{}\n... [{} entries elided] ...\n{}",
        head.join("\n"),
        lines.len() - 30,
        tail.join("\n")
    )
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{}…", head)
}

// ─── verdict parsing ────────────────────────────────────────────────────

pub fn parse_verdict(text: &str) -> Result<JudgeVerdict, JudgeError> {
    let json_str = extract_first_object(text).unwrap_or_else(|| text.trim().to_string());
    let v: JudgeVerdict = serde_json::from_str(&json_str)
        .map_err(|e| JudgeError::BadResponse(format!("not JSON: {e}; got: {text}")))?;
    Ok(JudgeVerdict {
        score: v.score.clamp(0.0, 1.0),
        ..v
    })
}

fn extract_first_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn build_provider(cfg: ProviderConfig, auth: AuthMethod) -> Box<dyn Provider> {
    match cfg.kind {
        ProviderKind::Anthropic => Box::new(AnthropicProvider::new(cfg, auth)),
        ProviderKind::OpenAi => Box::new(OpenAiProvider::new(cfg, auth)),
        ProviderKind::OpenAiCompat => Box::new(OpenAiCompatProvider::new(cfg, auth)),
        ProviderKind::Google => Box::new(GoogleProvider::new(cfg, auth)),
        ProviderKind::Bedrock => Box::new(BedrockAnthropicProvider::new(cfg, auth)),
        ProviderKind::Azure => Box::new(AzureOpenAiProvider::new(cfg, auth)),
    }
}
