//! TOML schema types for a `pi --orchestrate` campaign file (RFD 0021).
//!
//! Field names match exactly the TOML field reference in §"Campaign schema (TOML)".

use serde::Deserialize;

/// Top-level campaign file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Campaign {
    /// Display name (not an identity key).
    pub name: String,

    /// Optional description echoed in the report header.
    #[serde(default)]
    pub description: String,

    /// Target branch that milestones are merged into.
    pub target_branch: String,

    /// Global defaults applied to every milestone.
    #[serde(default)]
    pub defaults: Defaults,

    /// Ordered list of milestones.
    #[serde(default)]
    pub milestones: Vec<Milestone>,
}

/// Global default settings for all milestones.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Subagent name for the reviewer role (default: "code-reviewer").
    #[serde(default = "default_reviewer")]
    pub reviewer: String,

    /// Maximum fix-loop iterations per milestone before marking `FAILED`.
    /// `0` means no fix loop (NEEDS_FIX always aborts the milestone).
    /// Default: 2.
    #[serde(default = "default_fix_loop_max")]
    pub fix_loop_max: u32,

    /// Maximum retries on transient `git push` failures.
    /// Default: 3.
    #[serde(default = "default_push_retry_max")]
    pub push_retry_max: u32,
}

fn default_reviewer() -> String {
    "code-reviewer".to_string()
}

const fn default_fix_loop_max() -> u32 {
    2
}

const fn default_push_retry_max() -> u32 {
    3
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            reviewer: default_reviewer(),
            fix_loop_max: default_fix_loop_max(),
            push_retry_max: default_push_retry_max(),
        }
    }
}

/// A single campaign milestone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Milestone {
    /// Unique identifier within the campaign.
    pub id: String,

    /// Branch that the implementer will push to.
    pub branch: String,

    /// Ids of milestones that must reach `MERGED` before this one becomes eligible.
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Assignment text pasted verbatim into the implementer's first turn.
    pub assignment: String,

    /// Implementer subagent name.  Required — no default.
    pub implementer: String,

    /// Per-milestone reviewer override.  Falls back to `defaults.reviewer`.
    pub reviewer: Option<String>,

    /// Per-milestone fix-loop override.  Falls back to `defaults.fix_loop_max`.
    pub fix_loop_max: Option<u32>,

    /// Override rules applied to reviewer Concerns bullets.
    #[serde(default)]
    pub override_rules: Vec<OverrideRule>,
}

impl Milestone {
    /// Effective reviewer name (milestone override or campaign default).
    pub fn effective_reviewer<'a>(&'a self, defaults: &'a Defaults) -> &'a str {
        self.reviewer.as_deref().unwrap_or(&defaults.reviewer)
    }

    /// Effective fix_loop_max (milestone override or campaign default).
    pub fn effective_fix_loop_max(&self, defaults: &Defaults) -> u32 {
        self.fix_loop_max.unwrap_or(defaults.fix_loop_max)
    }
}

/// A single override rule inside a milestone.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideRule {
    /// Regex pattern matched against each Concerns bullet text.
    pub r#match: String,

    /// `"in-scope"` or `"out-of-scope"`.
    pub verdict: String,

    /// Target milestone id for forwarding (required when `verdict = "out-of-scope"`).
    pub forward_to: Option<String>,
}
