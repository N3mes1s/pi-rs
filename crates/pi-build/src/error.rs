//! Per RFD 0028 §A.6. Every variant carries the offending
//! input so the CLI's `pi-build validate` output names what
//! the operator wrote, not just the rule.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest schema_version {found} is newer than this pi-build supports (max {supported}); upgrade pi-build")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("manifest schema_version {found} is older than v1 (no v0 schema exists)")]
    SchemaTooOld { found: u32 },

    #[error("could not detect schema_version: {0}")]
    VersionDetect(toml::de::Error),

    #[error("manifest parse error: {0}")]
    Parse(toml::de::Error),

    #[error("invalid agent.name {0:?}: must match ^[a-z][a-z0-9_-]{{0,63}}$")]
    InvalidAgentName(String),

    #[error("invalid agent.description length {len} (must be 1..=1024 UTF-8 bytes)")]
    InvalidDescription { len: usize },

    #[error("invalid agent.version {0:?}: {1}")]
    InvalidVersion(String, semver::Error),

    #[error("invalid provider.model length {len} (must be 1..=256 UTF-8 bytes)")]
    InvalidModelLen { len: usize },

    #[error("invalid env-var name {0:?} in secrets.required: must match ^[A-Z][A-Z0-9_]*$")]
    InvalidEnvVarName(String),

    #[error("unknown tool {0:?} in tools.allowlist (v1 supports: read, write, edit, bash, grep, find, ls, web_search)")]
    UnknownTool(String),

    #[error("tool {0:?} is unsafe but tools.disallow_unsafe = true")]
    UnsafeToolWithDisallow(String),

    #[error("tools.allowlist is empty after dedup")]
    EmptyAllowlist,

    #[error("invalid runtime.system_prompt length {len} (must be 1..=65_536 UTF-8 bytes)")]
    InvalidSystemPromptLen { len: usize },

    #[error("runtime.max_session_tokens {found} below floor 1_000")]
    MaxSessionTokensTooLow { found: u64 },

    #[error("runtime.max_tool_invocations_per_turn must be >= 1")]
    MaxInvocationsTooLow,

    #[error("runtime.max_recursion {found} out of range 1..=16")]
    MaxRecursionOutOfRange { found: u64 },

    #[error("runtime.{field} = {found} exceeds usize::MAX on this host")]
    OutOfRangeForUsize {
        field: &'static str,
        found: u64,
    },
}
