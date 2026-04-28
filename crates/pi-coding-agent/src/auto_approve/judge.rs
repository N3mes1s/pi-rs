//! Model-based "auto" judge — Claude Code style, isolated from main agent.
//!
//! The judge consults a configurable cheap model (default
//! `claude-haiku-4-5`) with **only** the tool name and JSON-serialised
//! input. It explicitly does NOT see:
//!
//! * the conversation history
//! * the assistant's text deltas / thinking
//! * any other tool results
//!
//! That isolation is the security boundary: the main agent has no surface
//! to talk the judge into approving something. The judge gets only what
//! the policy layer would have seen, plus a fixed adversarial system
//! prompt.
//!
//! The judge replies with a single JSON object:
//! ```json
//! { "decision": "approve" | "reject", "reason": "one sentence" }
//! ```
//!
//! Anything else — wrong shape, extra prose around the JSON, parse failure,
//! provider error, timeout — counts as a **reject**. Fail-closed.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use pi_ai::{
    AnthropicProvider, AuthMethod, AuthStorage, AzureOpenAiProvider, BedrockAnthropicProvider,
    GenerateRequest, GoogleProvider, Message, ModelInfo, ModelRegistry, OpenAiCompatProvider,
    OpenAiProvider, Provider, ProviderConfig, ProviderKind, ThinkingLevel,
};

const SYSTEM_PROMPT: &str = "\
You are a security-critical tool-use auditor for an autonomous coding agent. \
For every request you will be given exactly one tool name and the JSON input \
the agent intends to pass to it. Decide whether running this tool with this \
input is safe.

You DO NOT see the conversation, the user's goal, or any prior tool results. \
You evaluate the call in isolation, on its merits.

Reject if the call could:
* delete, modify, or exfiltrate data outside the project working directory
* execute privileged operations (sudo, mount, mkfs, dd-if=)
* download-and-execute (`curl … | sh`, `wget … | bash`)
* send sensitive data over the network
* modify system files (/etc, /usr, /bin, .git/objects, .ssh/, ~/.aws/)
* run untrusted code with elevated permissions
* fork/spawn long-running daemons that escape the session

Approve only the most clearly benign cases. When in doubt, REJECT.

Reply with a SINGLE JSON object on one line and nothing else:
{\"decision\": \"approve\" | \"reject\", \"reason\": \"<one short sentence>\"}";

const JUDGE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeConfig {
    /// Provider name registered in `pi_ai::ModelRegistry` (e.g. "anthropic").
    pub provider: String,
    /// Model id or alias (e.g. "claude-haiku-4-5-20251001" or "haiku").
    pub model: String,
    /// Defaults to `Off`. The judge's job is structured-output, not reasoning.
    #[serde(default)]
    pub thinking: ThinkingLevel,
    /// Cap on judge response tokens.
    #[serde(default = "default_max_tokens")]
    pub max_output_tokens: u32,
}

fn default_max_tokens() -> u32 {
    256
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            thinking: ThinkingLevel::Off,
            max_output_tokens: 256,
        }
    }
}

/// Judge handle. Cloning is cheap (Arc internally).
#[derive(Clone)]
pub struct Judge {
    config: JudgeConfig,
    provider: std::sync::Arc<dyn Provider>,
    model_info: ModelInfo,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeVerdict {
    Approve,
    Reject(String),
}

impl Judge {
    /// Build a judge from a registry + auth + config. Returns
    /// `JudgeError::UnknownModel` or `MissingAuth` if it can't be wired.
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

    /// Run the judge against a single tool call. Always fail-closed.
    pub async fn judge(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<JudgeVerdict, JudgeError> {
        // Build the user message in a fixed structured shape so the main
        // agent can never inject prose into it.
        let payload = serde_json::json!({
            "tool_name": tool_name,
            "tool_input": tool_input,
        });
        let user_text = format!(
            "Audit this tool call:\n```json\n{}\n```",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );

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
        let resp = match tokio::time::timeout(JUDGE_TIMEOUT, provider.generate(req, &model)).await {
            Err(_) => return Err(JudgeError::Timeout),
            Ok(Err(e)) => return Err(JudgeError::Provider(e.to_string())),
            Ok(Ok(r)) => r,
        };

        let text = resp.message.text();
        parse_verdict(&text)
    }
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

/// Parse the judge model's reply. Strict: must be a JSON object with
/// `decision` ∈ {"approve", "reject"}. Anything else → reject.
pub fn parse_verdict(text: &str) -> Result<JudgeVerdict, JudgeError> {
    // Models sometimes add backtick fences or prose. Try to extract the
    // first {...} balanced object.
    let json_str = extract_first_object(text).unwrap_or(text.trim().to_string());
    let v: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| JudgeError::BadResponse(format!("not JSON: {e}; got: {text}")))?;
    let decision = v
        .get("decision")
        .and_then(|d| d.as_str())
        .ok_or_else(|| JudgeError::BadResponse(format!("missing decision: {text}")))?;
    let reason = v
        .get("reason")
        .and_then(|r| r.as_str())
        .unwrap_or("(no reason)")
        .to_string();
    match decision {
        "approve" => Ok(JudgeVerdict::Approve),
        "reject" => Ok(JudgeVerdict::Reject(reason)),
        other => Err(JudgeError::BadResponse(format!(
            "decision must be approve|reject, got `{other}`"
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let v = parse_verdict(r#"{"decision":"approve","reason":"safe"}"#).unwrap();
        assert_eq!(v, JudgeVerdict::Approve);
    }

    #[test]
    fn parse_with_backticks_and_prose() {
        let r = parse_verdict(
            "Sure, here's my judgement:\n```json\n{\"decision\": \"reject\", \"reason\": \"sudo invoked\"}\n```\nthanks!",
        )
        .unwrap();
        assert_eq!(r, JudgeVerdict::Reject("sudo invoked".into()));
    }

    #[test]
    fn unknown_decision_is_bad_response() {
        let err = parse_verdict(r#"{"decision":"maybe","reason":"idk"}"#).unwrap_err();
        assert!(matches!(err, JudgeError::BadResponse(_)));
    }

    #[test]
    fn missing_decision_is_bad_response() {
        let err = parse_verdict(r#"{"reason":"hmm"}"#).unwrap_err();
        assert!(matches!(err, JudgeError::BadResponse(_)));
    }

    #[test]
    fn not_json_is_bad_response() {
        let err = parse_verdict("approve, looks fine").unwrap_err();
        assert!(matches!(err, JudgeError::BadResponse(_)));
    }

    #[test]
    fn extract_first_object_handles_nested_and_strings() {
        let s = "noise {\"a\": {\"nested\": \"}\\\"\"}, \"b\": 1} trailing";
        let extracted = extract_first_object(s).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extracted).unwrap();
        assert_eq!(v["a"]["nested"], "}\"");
        assert_eq!(v["b"], 1);
    }

    #[test]
    fn judge_default_config_uses_haiku() {
        let cfg = JudgeConfig::default();
        assert_eq!(cfg.provider, "anthropic");
        assert!(cfg.model.starts_with("claude-haiku"));
        assert_eq!(cfg.max_output_tokens, 256);
    }
}
