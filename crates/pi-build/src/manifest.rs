//! Per RFD 0028 §A.3 — canonical serde surface for `agent.toml` v1.
//!
//! Defaults match pi-sdk's `RuntimeConfig::default()` so an
//! omitted block produces the same caps a hand-built embedder
//! would get (verified `pi-agent-core/src/runtime.rs:445-447`).
//!
//! `max_tool_invocations_per_turn` and `max_recursion` are
//! `u64` on the wire (platform-portable) but pi-sdk's runtime
//! fields are `usize`. Commit B's codegen lowers via
//! `usize::try_from(n)?`; Commit A's `validate()` runs the
//! same check up front and surfaces `OutOfRangeForUsize`.

use serde::{Deserialize, Serialize};

/// Top-level manifest, v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub agent: AgentMeta,
    pub provider: ProviderConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMeta {
    pub name: String,
    pub description: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub name: ProviderName,
    pub model: String,
    #[serde(default)]
    pub thinking: ThinkingLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderName {
    Anthropic,
    Openai,
    OpenaiCompat,
    Google,
    Bedrock,
    AzureOpenai,
}

impl ProviderName {
    /// Wire form pi-sdk's `Settings.provider: String` accepts
    /// (verified `pi-ai/src/auth.rs:102` ENV_KEYS table).
    pub fn as_kebab(self) -> &'static str {
        match self {
            ProviderName::Anthropic => "anthropic",
            ProviderName::Openai => "openai",
            ProviderName::OpenaiCompat => "openai-compat",
            ProviderName::Google => "google",
            ProviderName::Bedrock => "bedrock",
            ProviderName::AzureOpenai => "azure-openai",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Off,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default = "default_tool_allowlist")]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub disallow_unsafe: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            allowlist: default_tool_allowlist(),
            disallow_unsafe: false,
        }
    }
}

fn default_tool_allowlist() -> Vec<String> {
    vec![
        "read".into(),
        "grep".into(),
        "find".into(),
        "ls".into(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub system_prompt: String,
    #[serde(default = "default_max_session_tokens")]
    pub max_session_tokens: u64,
    #[serde(default = "default_max_tool_invocations_per_turn")]
    pub max_tool_invocations_per_turn: u64,
    #[serde(default = "default_max_recursion")]
    pub max_recursion: u64,
}

fn default_max_session_tokens() -> u64 {
    10_000_000
}
fn default_max_tool_invocations_per_turn() -> u64 {
    64
}
fn default_max_recursion() -> u64 {
    8
}

/// The 8 built-in tools v1 supports (per RFD 0028 §A.4).
pub const KNOWN_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "bash",
    "grep",
    "find",
    "ls",
    "web_search",
];

/// The 3 unsafe tools (`tools.disallow_unsafe = true` rejects them).
pub const UNSAFE_TOOLS: &[&str] = &["bash", "write", "edit"];
