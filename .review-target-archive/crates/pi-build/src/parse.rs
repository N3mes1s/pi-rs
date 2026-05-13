//! Per RFD 0028 §A.5 (two-pass parser) + §A.4 (semantic validation).
//!
//! The two-pass shape exists because `#[serde(deny_unknown_fields)]`
//! conflates "schema version too new" with "typo in v1 key" — pass 1
//! reads only `schema_version` from a permissive shim; pass 2 (only
//! if version matches v1) does the strict parse.

use serde::Deserialize;
use std::collections::BTreeSet;

use crate::error::ManifestError;
use crate::manifest::{Manifest, KNOWN_TOOLS, UNSAFE_TOOLS};

const SUPPORTED_SCHEMA_VERSION: u32 = 1;
const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_MODEL_BYTES: usize = 256;
const MAX_SYSTEM_PROMPT_BYTES: usize = 65_536;
const MIN_MAX_SESSION_TOKENS: u64 = 1_000;
const MAX_RECURSION_CEILING: u64 = 16;

#[derive(Deserialize)]
struct VersionShim {
    schema_version: u32,
    // No deny_unknown_fields — pass 1 ignores everything else.
}

/// Parse an `agent.toml` source string into a validated `Manifest`.
///
/// On success the returned `Manifest` has its `tools.allowlist`
/// already de-duplicated (per A.4 dedup behavior).
pub fn parse(raw: &str) -> Result<Manifest, ManifestError> {
    // PASS 1: detect schema_version with a permissive shim.
    let v: VersionShim = toml::from_str(raw).map_err(ManifestError::VersionDetect)?;
    match v.schema_version {
        SUPPORTED_SCHEMA_VERSION => {}
        0 => return Err(ManifestError::SchemaTooOld { found: 0 }),
        n => {
            return Err(ManifestError::SchemaTooNew {
                found: n,
                supported: SUPPORTED_SCHEMA_VERSION,
            })
        }
    }
    // PASS 2: strict v1 parse with deny_unknown_fields.
    let mut m: Manifest = toml::from_str(raw).map_err(ManifestError::Parse)?;
    validate(&mut m)?;
    Ok(m)
}

/// Semantic validation per A.4. Mutable to apply the silent
/// `tools.allowlist` dedup before returning to the caller.
pub fn validate(m: &mut Manifest) -> Result<(), ManifestError> {
    // agent.name regex: ^[a-z][a-z0-9_-]{0,63}$
    if !is_valid_agent_name(&m.agent.name) {
        return Err(ManifestError::InvalidAgentName(m.agent.name.clone()));
    }

    // agent.description length: 1..=1024 bytes
    let dlen = m.agent.description.len();
    if dlen == 0 || dlen > MAX_DESCRIPTION_BYTES {
        return Err(ManifestError::InvalidDescription { len: dlen });
    }

    // agent.version: SemVer
    semver::Version::parse(&m.agent.version)
        .map_err(|e| ManifestError::InvalidVersion(m.agent.version.clone(), e))?;

    // provider.model length: 1..=256 bytes
    let mlen = m.provider.model.len();
    if mlen == 0 || mlen > MAX_MODEL_BYTES {
        return Err(ManifestError::InvalidModelLen { len: mlen });
    }

    // secrets.required: each entry matches ^[A-Z][A-Z0-9_]*$
    for env in &m.secrets.required {
        if !is_valid_env_var_name(env) {
            return Err(ManifestError::InvalidEnvVarName(env.clone()));
        }
    }

    // tools.allowlist: silent dedup preserving insertion order
    // (BTreeSet membership; `Vec::retain` preserves position).
    {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        m.tools.allowlist.retain(|x| seen.insert(x.clone()));
    }

    // tools.allowlist non-empty after dedup
    if m.tools.allowlist.is_empty() {
        return Err(ManifestError::EmptyAllowlist);
    }

    // every tool in known set, case-sensitive lowercase only
    for t in &m.tools.allowlist {
        if !KNOWN_TOOLS.contains(&t.as_str()) {
            return Err(ManifestError::UnknownTool(t.clone()));
        }
    }

    // disallow_unsafe: reject overlap with UNSAFE_TOOLS
    if m.tools.disallow_unsafe {
        for t in &m.tools.allowlist {
            if UNSAFE_TOOLS.contains(&t.as_str()) {
                return Err(ManifestError::UnsafeToolWithDisallow(t.clone()));
            }
        }
    }

    // runtime.system_prompt length: 1..=65_536 bytes
    let slen = m.runtime.system_prompt.len();
    if slen == 0 || slen > MAX_SYSTEM_PROMPT_BYTES {
        return Err(ManifestError::InvalidSystemPromptLen { len: slen });
    }

    // runtime.max_session_tokens floor
    if m.runtime.max_session_tokens < MIN_MAX_SESSION_TOKENS {
        return Err(ManifestError::MaxSessionTokensTooLow {
            found: m.runtime.max_session_tokens,
        });
    }

    // runtime.max_tool_invocations_per_turn >= 1
    if m.runtime.max_tool_invocations_per_turn == 0 {
        return Err(ManifestError::MaxInvocationsTooLow);
    }

    // runtime.max_recursion: 1..=16
    if m.runtime.max_recursion == 0 || m.runtime.max_recursion > MAX_RECURSION_CEILING {
        return Err(ManifestError::MaxRecursionOutOfRange {
            found: m.runtime.max_recursion,
        });
    }

    // usize range check (no-op on 64-bit; meaningful on 32-bit)
    usize::try_from(m.runtime.max_tool_invocations_per_turn).map_err(|_| {
        ManifestError::OutOfRangeForUsize {
            field: "max_tool_invocations_per_turn",
            found: m.runtime.max_tool_invocations_per_turn,
        }
    })?;
    usize::try_from(m.runtime.max_recursion).map_err(|_| {
        ManifestError::OutOfRangeForUsize {
            field: "max_recursion",
            found: m.runtime.max_recursion,
        }
    })?;

    Ok(())
}

fn is_valid_agent_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if s.len() > 64 {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn is_valid_env_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !first.is_ascii_uppercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}
