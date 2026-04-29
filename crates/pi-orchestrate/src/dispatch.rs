//! Subprocess spawn primitive for the orchestrator runner.
//!
//! Invokes the `pi` binary as a subprocess (via `std::env::current_exe()`,
//! so the orchestrator dispatches its own image — no PATH lookup, no
//! version skew). The subagent definition supplies model + thinking
//! overrides; the assignment is fed on stdin; output streams come back
//! as the captured stdout (the subprocess runs in `-p` print mode,
//! which prints the final assistant message and exits).
//!
//! v1 design notes:
//!
//!   * Subprocess > in-process. We use std::process::Command rather
//!     than wiring native::task::executor::run_one because run_one
//!     requires a ParentHandle (parent_session + parent_cfg) that the
//!     orchestrator's own startup never builds. Subprocess isolation
//!     also matches how the orchestrator is being driven manually
//!     (nohup pi & nohup pi & ...) — same dispatch model.
//!
//!   * No streaming yet. v1 captures the FULL stdout into a String and
//!     hands it to the verdict parser. v2 may switch to a streaming
//!     reader if first-token-latency matters for live-display.
//!
//!   * Auth + worktree are inherited from the orchestrator's
//!     environment. The CWD passed to the subprocess is the
//!     milestone's branch checkout (the runner is responsible for
//!     `git checkout` before calling `dispatch`); v1 does NOT use a
//!     per-milestone worktree (RFD 0021 §"Concurrency" punts that to
//!     parallel mode, which v1 doesn't support).

use crate::schema::Milestone;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// Result of one subagent dispatch.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    /// Subagent name (e.g. "router-implementer", "code-reviewer").
    pub agent: String,
    /// Whether the process exited 0.
    pub success: bool,
    /// Full captured stdout — the print-mode subprocess prints the final
    /// assistant message text and exits.
    pub model_output: String,
    /// Captured stderr — only populated on non-zero exit; otherwise
    /// empty. Lets the runner attach it to the FAILED state event.
    pub stderr: String,
    /// Exit code, or 137 if the runner killed the process.
    pub exit_code: i32,
    /// Wall time elapsed from spawn to exit.
    pub duration_ms: u64,
}

/// What kind of subagent to dispatch. The orchestrator only ever
/// invokes implementer or reviewer in v1.
#[derive(Debug, Clone, Copy)]
pub enum DispatchRole {
    Implementer,
    Reviewer,
}

impl DispatchRole {
    fn label(self) -> &'static str {
        match self {
            DispatchRole::Implementer => "implementer",
            DispatchRole::Reviewer => "reviewer",
        }
    }
}

/// Trait so tests can mock the spawn without a real subprocess.
/// The default impl is `RealDispatch`; tests use a fake.
pub trait Dispatch {
    fn dispatch(
        &self,
        role: DispatchRole,
        agent_name: &str,
        assignment: &str,
        cwd: &Path,
    ) -> std::io::Result<DispatchOutcome>;
}

/// Production dispatcher: spawns `pi` as a subprocess.
pub struct RealDispatch {
    /// Path to the pi binary. Defaults to `current_exe()`. Override in
    /// tests so we can swap in a fake binary that prints a canned
    /// verdict.
    pub pi_binary: PathBuf,
}

impl Default for RealDispatch {
    fn default() -> Self {
        Self {
            pi_binary: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pi")),
        }
    }
}

impl Dispatch for RealDispatch {
    fn dispatch(
        &self,
        role: DispatchRole,
        agent_name: &str,
        assignment: &str,
        cwd: &Path,
    ) -> std::io::Result<DispatchOutcome> {
        let started = Instant::now();
        // The subagent name maps to a `.pi/agents/<name>.md` file the
        // pi binary already discovers; we rely on the binary's own
        // agent loader rather than re-parsing definitions here. v1
        // passes the agent name verbatim as the prompt-template id —
        // the implementer/reviewer subagent files set up their own
        // system prompts.
        //
        // For v1 we do NOT yet reach into the agent's own model+
        // thinking frontmatter; we let the user invoke pi with
        // whatever defaults their environment has set. v2 will read
        // the agent file directly and pass `-m <provider/model>
        // --thinking <level>`.
        let mut cmd = Command::new(&self.pi_binary);
        cmd.arg("-p")
            .arg("--auto-approve")
            .arg("auto-judge")
            .arg(format!(
                "[orchestrate-{}/{}] {}",
                role.label(),
                agent_name,
                assignment_first_line(assignment)
            ))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // PI_NO_SYNC keeps the subprocess from racing on shared
            // session-history mutations against the orchestrator's
            // own session.
            .env("PI_NO_SYNC", "1")
            // PI_ORCHESTRATE_ROLE is informational — lets the
            // subagent's system prompt branch on whether it's the
            // implementer or reviewer in case a single agent file
            // serves both roles.
            .env("PI_ORCHESTRATE_ROLE", role.label());

        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            // Whole assignment goes on stdin so the print-mode
            // implementation (modes/print.rs) treats it as the user
            // prompt. The first-line preview also went onto argv
            // above, but that's just for ps-listing readability;
            // stdin is the payload.
            stdin.write_all(assignment.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        let exit_code = output.status.code().unwrap_or(-1);
        Ok(DispatchOutcome {
            agent: agent_name.to_string(),
            success: output.status.success(),
            model_output: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: if output.status.success() {
                String::new()
            } else {
                String::from_utf8_lossy(&output.stderr).into_owned()
            },
            exit_code,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Truncate to the first non-empty line, capped at 80 chars. Used for
/// the argv preview so a `ps` listing shows what the agent is doing
/// without pulling the full multi-paragraph assignment into argv (some
/// kernels cap argv length, plus it makes process tables unreadable).
fn assignment_first_line(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.chars().take(80).collect()
}

/// Resolve which agent to dispatch for a given (milestone, role) tuple.
/// Returns the subagent name from the milestone's per-role override,
/// falling back to the campaign default for reviewer.
pub fn agent_for(role: DispatchRole, milestone: &Milestone, default_reviewer: &str) -> String {
    match role {
        DispatchRole::Implementer => milestone.implementer.clone(),
        DispatchRole::Reviewer => milestone
            .reviewer
            .clone()
            .unwrap_or_else(|| default_reviewer.to_string()),
    }
}
