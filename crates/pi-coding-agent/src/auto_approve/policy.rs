//! Deterministic per-tool allow / deny / ask rules.
//!
//! A [`Policy`] is a list of [`ToolRule`]s evaluated in order. The first
//! matching rule wins; if nothing matches, the default falls through to
//! [`Decision::Ask`].
//!
//! Schema (JSON, lives at `~/.pi/agent/auto-approve.json`):
//!
//! ```json
//! {
//!   "default_decision": "ask",
//!   "rules": [
//!     { "tool": "read",  "always_approve": true },
//!     { "tool": "ls",    "always_approve": true },
//!     { "tool": "grep",  "always_approve": true },
//!     { "tool": "find",  "always_approve": true },
//!     { "tool": "bash",
//!       "command_allow_regex": [
//!         "^cargo (build|test|run|check|clippy|fmt)( |$)",
//!         "^git (status|log|diff|show|branch|stash list)( |$)",
//!         "^(ls|pwd|whoami|date|env|which) ",
//!         "^echo ",
//!         "^cat [^|]+$"
//!       ],
//!       "command_deny_regex": [
//!         "(?:^|[ ;&|`$])sudo[ \t]",
//!         "(?:^|[ ;&|`$])rm[ \t]+(-[a-zA-Z]*r[a-zA-Z]*[ \t]+)?(/|~)",
//!         "(?:^|[ ;&|`$])mkfs",
//!         "(?:^|[ ;&|`$])dd[ \t]+if=",
//!         ":\\(\\)\\s*\\{\\s*:",
//!         "(?:^|[ ;&|`$])(curl|wget)[^|]*\\|[ \t]*sh"
//!       ]
//!     },
//!     { "tool": "write",
//!       "path_allow_globs": ["**/*.rs", "**/*.md", "**/*.toml", "**/*.json"],
//!       "path_deny_globs": ["/etc/**", "/usr/**", "/bin/**", "**/.git/**"]
//!     },
//!     { "tool": "edit", "inherit_from": "write" }
//!   ]
//! }
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("invalid regex: {0}")]
    BadRegex(#[from] regex::Error),
    #[error("invalid glob: {0}")]
    BadGlob(#[from] glob::PatternError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

/// A single per-tool rule. Rules use `tool` as the match key. Wildcard
/// `"*"` matches every tool not named by another rule.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolRule {
    pub tool: String,

    /// Approve every call without further checks.
    #[serde(default)]
    pub always_approve: bool,

    /// Reject every call.
    #[serde(default)]
    pub always_deny: bool,

    /// (bash only) regex patterns; if any matches the `command` input,
    /// approve. Evaluated AFTER `command_deny_regex` so deny wins on overlap.
    #[serde(default)]
    pub command_allow_regex: Vec<String>,

    /// (bash only) regex patterns; if any matches, reject regardless.
    #[serde(default)]
    pub command_deny_regex: Vec<String>,

    /// (write/edit) path globs matched against the `path` input. Approve
    /// if matches AND no deny matches.
    #[serde(default)]
    pub path_allow_globs: Vec<String>,

    /// (write/edit) path globs that always reject.
    #[serde(default)]
    pub path_deny_globs: Vec<String>,

    /// Copy the matching rule's other fields. Useful for `edit` ↔ `write`.
    #[serde(default)]
    pub inherit_from: Option<String>,
}

impl ToolRule {
    pub fn allow(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            always_approve: true,
            ..Default::default()
        }
    }
    pub fn deny(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            always_deny: true,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultDecision {
    Approve,
    Reject,
    Ask,
}

impl Default for DefaultDecision {
    fn default() -> Self {
        Self::Ask
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub default_decision: DefaultDecision,
    #[serde(default)]
    pub rules: Vec<ToolRule>,
}

/// Final per-call decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject(String),
    /// Defer to the next layer (judge or user prompt).
    Ask,
}

impl Policy {
    /// A "safe defaults" policy used when no `auto-approve.json` exists.
    /// Read-only tools auto-approve; everything else asks.
    pub fn default_safe() -> Self {
        Self {
            default_decision: DefaultDecision::Ask,
            rules: vec![
                ToolRule::allow("read"),
                ToolRule::allow("ls"),
                ToolRule::allow("grep"),
                ToolRule::allow("find"),
            ],
        }
    }

    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let txt = std::fs::read_to_string(path)?;
        let p: Self = serde_json::from_str(&txt)?;
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<(), PolicyError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let txt = serde_json::to_string_pretty(self)?;
        std::fs::write(path, txt)?;
        Ok(())
    }

    /// Resolve `inherit_from` references in-place. Idempotent.
    pub fn resolve_inheritance(&mut self) {
        let snapshot: BTreeMap<String, ToolRule> = self
            .rules
            .iter()
            .map(|r| (r.tool.clone(), r.clone()))
            .collect();
        for r in &mut self.rules {
            let Some(parent_name) = r.inherit_from.clone() else { continue };
            let Some(parent) = snapshot.get(&parent_name) else { continue };
            if r.command_allow_regex.is_empty() {
                r.command_allow_regex = parent.command_allow_regex.clone();
            }
            if r.command_deny_regex.is_empty() {
                r.command_deny_regex = parent.command_deny_regex.clone();
            }
            if r.path_allow_globs.is_empty() {
                r.path_allow_globs = parent.path_allow_globs.clone();
            }
            if r.path_deny_globs.is_empty() {
                r.path_deny_globs = parent.path_deny_globs.clone();
            }
            if !r.always_approve {
                r.always_approve = parent.always_approve;
            }
            if !r.always_deny {
                r.always_deny = parent.always_deny;
            }
        }
    }

    /// Evaluate a single tool call. The first matching rule wins (`tool ==
    /// name` or `tool == "*"`). Within a matching rule, deny conditions
    /// take precedence over allow conditions.
    pub fn evaluate(&self, tool_name: &str, input: &Value) -> Decision {
        // Find rules in order: exact match first, then wildcard.
        let exact = self.rules.iter().find(|r| r.tool == tool_name);
        let wildcard = self.rules.iter().find(|r| r.tool == "*");
        let rule = exact.or(wildcard);

        if let Some(rule) = rule {
            // 1) blanket reject
            if rule.always_deny {
                return Decision::Reject(format!(
                    "policy: tool `{tool_name}` denied by always_deny rule"
                ));
            }
            // 2) bash command regex
            if !rule.command_deny_regex.is_empty() || !rule.command_allow_regex.is_empty() {
                let cmd = input
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                for re in &rule.command_deny_regex {
                    if let Ok(r) = Regex::new(re) {
                        if r.is_match(cmd) {
                            return Decision::Reject(format!(
                                "policy: bash command rejected by deny pattern `{re}`"
                            ));
                        }
                    }
                }
                for re in &rule.command_allow_regex {
                    if let Ok(r) = Regex::new(re) {
                        if r.is_match(cmd) {
                            return Decision::Approve;
                        }
                    }
                }
                // command had allow rules but didn't match any → ask
                if !rule.command_allow_regex.is_empty() {
                    return Decision::Ask;
                }
            }
            // 3) write/edit path globs
            if !rule.path_deny_globs.is_empty() || !rule.path_allow_globs.is_empty() {
                let path = input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                for g in &rule.path_deny_globs {
                    if let Ok(p) = glob::Pattern::new(g) {
                        if p.matches(path) {
                            return Decision::Reject(format!(
                                "policy: path rejected by deny glob `{g}`"
                            ));
                        }
                    }
                }
                for g in &rule.path_allow_globs {
                    if let Ok(p) = glob::Pattern::new(g) {
                        if p.matches(path) {
                            return Decision::Approve;
                        }
                    }
                }
                if !rule.path_allow_globs.is_empty() {
                    return Decision::Ask;
                }
            }
            // 4) blanket approve
            if rule.always_approve {
                return Decision::Approve;
            }
        }
        match self.default_decision {
            DefaultDecision::Approve => Decision::Approve,
            DefaultDecision::Reject => Decision::Reject(format!(
                "policy: default_decision=reject; no rule matches tool `{tool_name}`"
            )),
            DefaultDecision::Ask => Decision::Ask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Policy {
        Policy::default_safe()
    }

    #[test]
    fn read_default_safe_approves_read_ls_grep_find() {
        for tool in ["read", "ls", "grep", "find"] {
            let d = p().evaluate(tool, &serde_json::json!({}));
            assert_eq!(d, Decision::Approve, "tool: {tool}");
        }
    }

    #[test]
    fn unknown_tool_falls_to_ask_under_default_safe() {
        let d = p().evaluate("bash", &serde_json::json!({"command": "ls"}));
        assert_eq!(d, Decision::Ask);
    }

    #[test]
    fn bash_command_allow_regex_approves() {
        let mut pol = Policy::default();
        pol.rules.push(ToolRule {
            tool: "bash".into(),
            command_allow_regex: vec!["^cargo (build|test)$".into()],
            ..Default::default()
        });
        assert_eq!(
            pol.evaluate("bash", &serde_json::json!({"command": "cargo build"})),
            Decision::Approve
        );
        assert_eq!(
            pol.evaluate("bash", &serde_json::json!({"command": "cargo doc"})),
            Decision::Ask
        );
    }

    #[test]
    fn bash_command_deny_wins_over_allow() {
        let mut pol = Policy::default();
        pol.rules.push(ToolRule {
            tool: "bash".into(),
            command_deny_regex: vec!["sudo".into()],
            command_allow_regex: vec![".*".into()], // would otherwise approve
            ..Default::default()
        });
        let d = pol.evaluate("bash", &serde_json::json!({"command": "sudo ls"}));
        assert!(matches!(d, Decision::Reject(_)));
    }

    #[test]
    fn write_path_deny_blocks() {
        let mut pol = Policy::default();
        pol.rules.push(ToolRule {
            tool: "write".into(),
            path_allow_globs: vec!["**/*.rs".into()],
            path_deny_globs: vec!["/etc/**".into()],
            ..Default::default()
        });
        assert!(matches!(
            pol.evaluate("write", &serde_json::json!({"path": "/etc/passwd"})),
            Decision::Reject(_)
        ));
        assert_eq!(
            pol.evaluate("write", &serde_json::json!({"path": "src/main.rs"})),
            Decision::Approve
        );
    }

    #[test]
    fn always_deny_overrides_everything() {
        let mut pol = Policy::default();
        pol.rules.push(ToolRule::deny("bash"));
        let d = pol.evaluate("bash", &serde_json::json!({"command": "ls"}));
        assert!(matches!(d, Decision::Reject(_)));
    }

    #[test]
    fn inherit_from_copies_unset_fields() {
        let mut pol = Policy::default();
        pol.rules.push(ToolRule {
            tool: "write".into(),
            path_allow_globs: vec!["**/*.rs".into()],
            ..Default::default()
        });
        pol.rules.push(ToolRule {
            tool: "edit".into(),
            inherit_from: Some("write".into()),
            ..Default::default()
        });
        pol.resolve_inheritance();
        let edit = pol.rules.iter().find(|r| r.tool == "edit").unwrap();
        assert_eq!(edit.path_allow_globs, vec!["**/*.rs".to_string()]);
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto-approve.json");
        let pol = Policy::default_safe();
        pol.save(&path).unwrap();
        let loaded = Policy::load(&path).unwrap();
        assert_eq!(loaded.rules.len(), pol.rules.len());
        assert_eq!(
            loaded.evaluate("read", &serde_json::json!({})),
            Decision::Approve
        );
    }

    #[test]
    fn missing_file_returns_io_error() {
        let err = Policy::load(Path::new("/nonexistent/foo.json")).unwrap_err();
        assert!(matches!(err, PolicyError::Io(_)));
    }

    #[test]
    fn wildcard_rule_catches_unmatched_tools() {
        let mut pol = Policy::default();
        pol.rules.push(ToolRule {
            tool: "*".into(),
            always_deny: true,
            ..Default::default()
        });
        let d = pol.evaluate("anything", &serde_json::json!({}));
        assert!(matches!(d, Decision::Reject(_)));
    }
}
