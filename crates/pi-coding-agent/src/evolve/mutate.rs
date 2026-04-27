//! Reflective mutation engine (G6).
//!
//! Rewrites ONE [`Section`] of an [`AgentsMd`] given evidence of recent
//! wins and losses. The slow model receives a structured prompt with:
//!
//! - The section heading + current body.
//! - Headings of all other sections (read-only context).
//! - Up to N best wins (user request + what worked).
//! - Up to N worst losses (user request + what failed).
//!
//! and returns a new body. Strict post-processing strips code fences,
//! enforces a length cap (default 1.2× current body), and rejects empty
//! output.
//!
//! Unlike random or crossover mutation, this is *targeted*: every change
//! is justified by concrete failure modes the section was implicated in.
//! Matches GEPA's reflective-mutation operator.
//!
//! The slow model is configurable; defaults to whatever
//! `Settings::roles::slow` resolves to (or the project default model when
//! unset).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider,
    BedrockAnthropicProvider, GenerateRequest, GoogleProvider, Message, ModelInfo,
    ModelRegistry, OpenAiCompatProvider, OpenAiProvider, Provider, ProviderConfig,
    ProviderKind, ThinkingLevel,
};

use super::agents_md::{AgentsMd, Section};

const SYSTEM_PROMPT: &str = "\
You are the curator of a project's AGENTS.md — the file the autonomous \
coding agent reads on every task. Your job is to rewrite ONE section so \
that the agent becomes more likely to succeed on tasks like the ones in \
<evidence>.

You see the section to rewrite, the headings of all other sections (for \
context — DO NOT modify them), and up to 3 wins and 3 losses from recent \
sessions in this project.

Rules:
- Do NOT change the section heading.
- Keep the new body within 120% of the current body's length.
- Address concrete failure modes evidenced in <losses>.
- Reinforce concrete habits evidenced in <wins>.
- Imperative voice. No apologies, no marketing prose, no \"I will\".
- Output ONLY the new body text. No preamble, no commentary, no code fences.";

const MUTATE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutatorConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub thinking: ThinkingLevel,
    /// Cap on response length. Defaults to 4096.
    #[serde(default = "default_max_tokens")]
    pub max_output_tokens: u32,
    /// Reject output longer than `length_cap_factor * current_body.len()`.
    /// Default 1.2.
    #[serde(default = "default_length_cap_factor")]
    pub length_cap_factor: f32,
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_length_cap_factor() -> f32 {
    1.2
}

impl Default for MutatorConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            thinking: ThinkingLevel::Off,
            max_output_tokens: default_max_tokens(),
            length_cap_factor: default_length_cap_factor(),
        }
    }
}

/// Win or loss from a past session, condensed for the prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub user_request: String,
    /// LLM-judge `reason` field, or heuristic notes when judge wasn't available.
    pub verdict_reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MutationEvidence {
    pub wins: Vec<EvidenceItem>,
    pub losses: Vec<EvidenceItem>,
}

#[derive(Debug, thiserror::Error)]
pub enum MutateError {
    #[error("section index {0} out of range")]
    IndexOutOfRange(usize),
    #[error("section is marked pi:keep and cannot be mutated")]
    Immutable,
    #[error("no evidence — at least one win or one loss is required")]
    NoEvidence,
    #[error("unknown mutator model: {0}")]
    UnknownModel(String),
    #[error("missing credentials for mutator provider {0}")]
    MissingAuth(String),
    #[error("mutator timed out after {}s", MUTATE_TIMEOUT.as_secs())]
    Timeout,
    #[error("provider error: {0}")]
    Provider(String),
    #[error("output exceeded length cap ({0} > {1} chars)")]
    LengthCapExceeded(usize, usize),
    #[error("output was empty after post-processing")]
    EmptyOutput,
}

#[derive(Clone)]
pub struct Mutator {
    config: MutatorConfig,
    provider: std::sync::Arc<dyn Provider>,
    model_info: ModelInfo,
}

impl Mutator {
    pub fn build(
        registry: &ModelRegistry,
        auth: &AuthStorage,
        config: MutatorConfig,
    ) -> Result<Self, MutateError> {
        let target = format!("{}/{}", config.provider, config.model);
        let (provider_cfg, model_info) = registry
            .resolve(&target)
            .or_else(|| registry.resolve(&config.model))
            .ok_or_else(|| MutateError::UnknownModel(target.clone()))?;
        let auth_method = auth
            .get(&provider_cfg.name)
            .ok_or_else(|| MutateError::MissingAuth(provider_cfg.name.clone()))?;
        let provider = build_provider(provider_cfg.clone(), auth_method);
        Ok(Self {
            config,
            provider: provider.into(),
            model_info: model_info.clone(),
        })
    }

    /// Mutate one section. Returns the new body (unchanged heading).
    /// Caller installs via `AgentsMd::replace_section`.
    pub async fn mutate_section(
        &self,
        doc: &AgentsMd,
        section_idx: usize,
        evidence: &MutationEvidence,
    ) -> Result<String, MutateError> {
        let section = doc
            .sections
            .get(section_idx)
            .ok_or(MutateError::IndexOutOfRange(section_idx))?;
        if !section.mutable {
            return Err(MutateError::Immutable);
        }
        if evidence.wins.is_empty() && evidence.losses.is_empty() {
            return Err(MutateError::NoEvidence);
        }

        let user_text = build_prompt(doc, section_idx, section, evidence);

        let req = GenerateRequest {
            model: self.model_info.id.clone(),
            system: Some(SYSTEM_PROMPT.into()),
            messages: vec![Message::user_text(user_text)],
            tools: Vec::new(),
            thinking: self.config.thinking,
            temperature: Some(0.2),
            max_output_tokens: Some(self.config.max_output_tokens),
            extras: serde_json::Value::Null,
        };

        let provider = self.provider.clone();
        let model = self.model_info.clone();
        let resp =
            match tokio::time::timeout(MUTATE_TIMEOUT, provider.generate(req, &model)).await {
                Err(_) => return Err(MutateError::Timeout),
                Ok(Err(e)) => return Err(MutateError::Provider(e.to_string())),
                Ok(Ok(r)) => r,
            };

        let raw = resp.message.text();
        post_process(&raw, section.body.len(), self.config.length_cap_factor)
    }
}

// ─── prompt assembly ────────────────────────────────────────────────────

pub fn build_prompt(
    doc: &AgentsMd,
    section_idx: usize,
    section: &Section,
    evidence: &MutationEvidence,
) -> String {
    let other_headings: Vec<String> = doc
        .sections
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != section_idx)
        .map(|(_, s)| s.heading.trim_end().to_string())
        .collect();

    let wins_block = evidence
        .wins
        .iter()
        .take(3)
        .map(|e| {
            format!(
                "- Task: {}\n  Verdict: {}",
                truncate(&e.user_request, 240),
                truncate(&e.verdict_reason, 240)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let losses_block = evidence
        .losses
        .iter()
        .take(3)
        .map(|e| {
            format!(
                "- Task: {}\n  Failure: {}",
                truncate(&e.user_request, 240),
                truncate(&e.verdict_reason, 240)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "<heading>\n{}\n</heading>\n\n\
         <current_body>\n{}\n</current_body>\n\n\
         <other_sections>\n{}\n</other_sections>\n\n\
         <wins>\n{}\n</wins>\n\n\
         <losses>\n{}\n</losses>\n\n\
         Rewrite ONLY <current_body>. Output the new body text and nothing else.",
        section.heading.trim_end(),
        section.body,
        if other_headings.is_empty() {
            "(none)".into()
        } else {
            other_headings.join("\n")
        },
        if wins_block.is_empty() {
            "(no wins observed)".into()
        } else {
            wins_block
        },
        if losses_block.is_empty() {
            "(no losses observed)".into()
        } else {
            losses_block
        },
    )
}

// ─── output post-processing ─────────────────────────────────────────────

/// Strip code fences, trim leading/trailing blank lines, enforce length
/// cap, reject empty.
pub fn post_process(
    raw: &str,
    current_len: usize,
    length_cap_factor: f32,
) -> Result<String, MutateError> {
    let stripped = strip_code_fences(raw);
    let trimmed = trim_blank_lines(&stripped);
    if trimmed.trim().is_empty() {
        return Err(MutateError::EmptyOutput);
    }
    let cap = (current_len as f32 * length_cap_factor) as usize;
    let cap = cap.max(64); // never below 64 chars even for tiny sections
    if trimmed.len() > cap {
        return Err(MutateError::LengthCapExceeded(trimmed.len(), cap));
    }
    let mut out = trimmed;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn strip_code_fences(s: &str) -> String {
    let t = s.trim();
    let lines: Vec<&str> = t.lines().collect();
    if lines.len() >= 2
        && lines[0].trim_start().starts_with("```")
        && lines.last().map_or(false, |l| l.trim() == "```")
    {
        return lines[1..lines.len() - 1].join("\n");
    }
    s.to_string()
}

fn trim_blank_lines(s: &str) -> String {
    let mut start = 0;
    let bytes = s.as_bytes();
    while start < bytes.len() && (bytes[start] == b'\n' || bytes[start] == b' ' || bytes[start] == b'\t') {
        if bytes[start] == b'\n' {
            start += 1;
        } else {
            // a non-empty leading line — keep its leading whitespace
            break;
        }
    }
    let mut end = s.len();
    while end > start {
        let last = &s[..end];
        if last.ends_with('\n') {
            // count back through trailing blank lines
            let lines: Vec<&str> = last.lines().collect();
            if lines.last().map_or(false, |l| l.trim().is_empty()) {
                end = last.rfind('\n').unwrap_or(0);
                continue;
            }
        }
        break;
    }
    s[start..end].to_string()
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{}…", head)
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
