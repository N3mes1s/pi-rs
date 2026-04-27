//! Auto-approval gate for tool calls — two-layer design.
//!
//! Every tool invocation flows through [`gate`] before [`Tool::invoke`] runs.
//! The gate has **two layers**, evaluated in order; the second is never
//! consulted unless the first explicitly defers.
//!
//! 1. **[`policy`] — deterministic, never bypassed.**
//!    A user-controlled JSON file at `~/.pi/agent/auto-approve.json`
//!    declares per-tool allow / deny / ask rules. Reads are
//!    [`Decision::Approve`] by default; [`Decision::Reject`] is final.
//!    The model's text output never reaches the policy evaluator.
//!
//! 2. **[`judge`] — optional, isolated, fail-closed.**
//!    When the policy returns [`Decision::Ask`] AND the active mode is
//!    `auto-judge`, a configurable cheap model is consulted with **only**:
//!      * the tool name + JSON-serialised input
//!      * a fixed adversarial system prompt
//!      * NO conversation history, NO assistant text, NO other tool results
//!    The judge replies with a single JSON object `{decision, reason}`;
//!    anything that doesn't parse cleanly counts as a reject. The main
//!    agent has no surface to influence the judge's input.
//!
//! Modes ([`Mode`]):
//!
//! | mode          | policy `Approve` | policy `Reject` | policy `Ask` |
//! |---------------|------------------|-----------------|--------------|
//! | `ask`         | run              | block + reason  | prompt user  |
//! | `auto-policy` | run              | block + reason  | prompt user  |
//! | `auto-judge`  | run              | block + reason  | judge → run/block |
//! | `yolo`        | run              | run (!)         | run          |
//!
//! `yolo` is the only mode that bypasses policy `Reject`. It exists for
//! controlled environments (CI sandboxes, autoresearch loops in disposable
//! cwds). It is never the default.

pub mod gate_adapter;
pub mod judge;
pub mod policy;

use serde::{Deserialize, Serialize};

pub use gate_adapter::AutoApproveGate;
pub use judge::{Judge, JudgeConfig};
pub use policy::{Decision, Policy, PolicyError, ToolRule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Every tool call requires user confirmation.
    Ask,
    /// Policy decides; `Ask` falls through to interactive confirmation.
    AutoPolicy,
    /// Policy decides; `Ask` is delegated to the judge model.
    AutoJudge,
    /// Approve everything. Disposable-environment escape hatch.
    Yolo,
}

impl Default for Mode {
    fn default() -> Self {
        Self::Ask
    }
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ask" => Self::Ask,
            "auto-policy" | "auto" => Self::AutoPolicy,
            "auto-judge" => Self::AutoJudge,
            "yolo" => Self::Yolo,
            _ => return None,
        })
    }
}

/// Final outcome handed back to the agent loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Tool may execute.
    Approve,
    /// Tool blocked. The reason is fed back to the model as a tool error
    /// so it knows why and can try a different approach.
    Reject(String),
    /// The gate cannot decide on its own; the caller (TUI / SDK host)
    /// should prompt the user. CI/headless callers should treat this as
    /// `Reject` for safety.
    AskUser(String),
}

impl Outcome {
    pub fn approved(&self) -> bool {
        matches!(self, Self::Approve)
    }
}

/// Single entry point for the agent loop. Call once per tool invocation
/// before running the tool.
///
/// `judge` is only consulted in [`Mode::AutoJudge`]; in every other mode
/// you may pass `None`.
pub async fn gate(
    mode: Mode,
    policy: &Policy,
    judge: Option<&Judge>,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> Outcome {
    let decision = policy.evaluate(tool_name, tool_input);
    match (mode, decision) {
        (_, Decision::Approve) => Outcome::Approve,
        (Mode::Yolo, _) => Outcome::Approve,
        (_, Decision::Reject(reason)) => Outcome::Reject(reason),
        (Mode::Ask, Decision::Ask) => {
            Outcome::AskUser(format!("policy says ASK for tool `{tool_name}`"))
        }
        (Mode::AutoPolicy, Decision::Ask) => {
            Outcome::AskUser(format!("policy says ASK for tool `{tool_name}`"))
        }
        (Mode::AutoJudge, Decision::Ask) => match judge {
            None => Outcome::Reject(
                "auto-judge mode active but no judge model configured".into(),
            ),
            Some(j) => match j.judge(tool_name, tool_input).await {
                Ok(judge::JudgeVerdict::Approve) => Outcome::Approve,
                Ok(judge::JudgeVerdict::Reject(reason)) => Outcome::Reject(reason),
                Err(e) => Outcome::Reject(format!("judge error (fail-closed): {e}")),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn yolo_approves_everything_including_policy_rejects() {
        let mut p = Policy::default();
        p.rules.push(ToolRule::deny("bash"));
        let out = gate(
            Mode::Yolo,
            &p,
            None,
            "bash",
            &serde_json::json!({"command": "rm -rf /"}),
        )
        .await;
        assert_eq!(out, Outcome::Approve);
    }

    #[tokio::test]
    async fn ask_returns_askuser_for_unmatched() {
        let p = Policy::default();
        let out = gate(
            Mode::Ask,
            &p,
            None,
            "bash",
            &serde_json::json!({"command": "ls"}),
        )
        .await;
        assert!(matches!(out, Outcome::AskUser(_)));
    }

    #[tokio::test]
    async fn auto_judge_no_judge_is_fail_closed_reject() {
        let p = Policy::default();
        let out = gate(Mode::AutoJudge, &p, None, "bash", &serde_json::json!({})).await;
        assert!(matches!(out, Outcome::Reject(_)));
    }

    #[tokio::test]
    async fn read_tool_default_approve() {
        let p = Policy::default_safe();
        let out = gate(
            Mode::Ask,
            &p,
            None,
            "read",
            &serde_json::json!({"path": "/tmp/x"}),
        )
        .await;
        assert_eq!(out, Outcome::Approve);
    }
}
