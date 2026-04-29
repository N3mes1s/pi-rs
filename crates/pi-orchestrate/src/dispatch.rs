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

/// Subset of a `.pi/agents/<name>.md` definition that the orchestrator
/// needs at dispatch time. We don't reuse pi-coding-agent's
/// `AgentDefinition` because pi-orchestrate is a leaf crate and a
/// circular dependency is the wrong fix.
///
/// The agent's YAML frontmatter looks like:
///   ---
///   name: code-reviewer
///   description: ...
///   model: openai-codex/gpt-5.4
///   thinking: high
///   tools: [...]
///   ---
///   <system prompt body>
///
/// We extract `model`, `thinking`, and the body. The body becomes the
/// system prompt; pi v1 has no `--system-prompt-file` flag, so we
/// prepend it to the assignment at dispatch time. v2 should add
/// `--system-prompt-file` and stop concatenating.
#[derive(Debug, Clone, Default)]
pub struct AgentSpec {
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub system_prompt: String,
}

/// Load `<repo_root>/.pi/agents/<name>.md`. v1 only resolves project-
/// local agents; user-global (`~/.pi/agents/`) and bundled fallbacks
/// are deferred to v2. Errors are propagated so the dispatcher can
/// emit a FAILED state event with a clear cause rather than silently
/// running with no system prompt.
pub fn load_agent_spec(repo_root: &Path, name: &str) -> std::io::Result<AgentSpec> {
    let path = repo_root
        .join(".pi")
        .join("agents")
        .join(format!("{name}.md"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("agent definition {} unreadable: {e}", path.display()),
        )
    })?;
    parse_agent_md(&text).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("agent definition {} missing frontmatter", path.display()),
        )
    })
}

/// Parse the markdown-with-frontmatter shape. Returns `None` if the
/// frontmatter delimiters are missing — caller treats as a failure.
fn parse_agent_md(text: &str) -> Option<AgentSpec> {
    // The file MUST open with `---\n` and contain a closing `---` line.
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    // Find the closing `---` line at the start of a line.
    let close_idx = rest.find("\n---\n").or_else(|| rest.find("\n---\r\n"))?;
    let frontmatter = &rest[..close_idx];
    // Body starts after the closing delimiter and the newline that
    // follows it.
    let body_start = close_idx + "\n---\n".len();
    let body = if body_start < rest.len() {
        &rest[body_start..]
    } else {
        ""
    };

    let mut spec = AgentSpec {
        model: None,
        thinking: None,
        system_prompt: body.trim_start_matches('\n').to_string(),
    };
    // Single-pass YAML-lite parse: only pull out top-level
    // `key: value` lines for model + thinking. Anything else is
    // ignored — we don't need tools/spawns/etc at dispatch time.
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "model" => spec.model = Some(value.to_string()),
            "thinking" => spec.thinking = Some(value.to_string()),
            _ => {}
        }
    }
    Some(spec)
}

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

        // B1 fix: actually use the named subagent.
        // Load `<cwd>/.pi/agents/<agent_name>.md` and apply its
        // model + thinking + system prompt to the spawned pi. The
        // previous v1 just put the agent_name in the argv preview
        // and ignored the file entirely, so every milestone ran
        // under the user's default agent regardless of the campaign
        // TOML's `implementer` / `reviewer` fields.
        //
        // pi v1 has no `--system-prompt-file` flag, so we prepend
        // the agent's body to the assignment (under a clear marker)
        // and let the spawned pi treat the whole thing as one user
        // prompt. v2 should add `--system-prompt-file` and stop
        // concatenating.
        let agent = load_agent_spec(cwd, agent_name).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("dispatch role={} agent={agent_name}: {e}", role.label()),
            )
        })?;
        let injected_prompt = if agent.system_prompt.is_empty() {
            assignment.to_string()
        } else {
            format!(
                "# Agent: {agent_name} (role: {role})\n\n{}\n\n---\n\n# Assignment\n\n{assignment}",
                agent.system_prompt.trim_end(),
                role = role.label(),
            )
        };

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
            .env("PI_NO_SYNC", "1")
            .env("PI_ORCHESTRATE_ROLE", role.label());

        // B1 fix continued: pass model + thinking from the agent's
        // YAML frontmatter. Without these, the spawned pi runs under
        // whatever model the parent's settings happen to default to.
        if let Some(model) = &agent.model {
            cmd.arg("-m").arg(model);
        }
        if let Some(thinking) = &agent.thinking {
            cmd.arg("--thinking").arg(thinking);
        }

        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(injected_prompt.as_bytes())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ─── B1: agent-spec parser ──────────────────────────────

    #[test]
    fn parse_agent_md_extracts_model_thinking_and_body() {
        let md = "---\n\
                  name: code-reviewer\n\
                  description: \"Senior pi-rs reviewer\"\n\
                  model: openai-codex/gpt-5.4\n\
                  thinking: high\n\
                  tools: [read, grep, bash]\n\
                  ---\n\
                  You are a senior Rust engineer reviewing a feature branch.\n\
                  \n\
                  Output a verdict ending with `Merge readiness: ...`.\n";
        let spec = parse_agent_md(md).expect("parses");
        assert_eq!(spec.model.as_deref(), Some("openai-codex/gpt-5.4"));
        assert_eq!(spec.thinking.as_deref(), Some("high"));
        assert!(spec.system_prompt.contains("senior Rust engineer"));
        assert!(spec.system_prompt.contains("Merge readiness"));
    }

    #[test]
    fn parse_agent_md_returns_none_without_frontmatter() {
        let no_frontmatter = "name: not-frontmatter\n\nbody text\n";
        assert!(parse_agent_md(no_frontmatter).is_none());
    }

    #[test]
    fn parse_agent_md_handles_quoted_values() {
        let md = "---\n\
                  model: \"openai/gpt-4o\"\n\
                  thinking: 'medium'\n\
                  ---\n\
                  body\n";
        let spec = parse_agent_md(md).expect("parses");
        assert_eq!(spec.model.as_deref(), Some("openai/gpt-4o"));
        assert_eq!(spec.thinking.as_deref(), Some("medium"));
    }

    #[test]
    fn parse_agent_md_omitted_fields_default_to_none() {
        let md = "---\n\
                  name: minimal\n\
                  ---\n\
                  system prompt body\n";
        let spec = parse_agent_md(md).expect("parses");
        assert!(spec.model.is_none());
        assert!(spec.thinking.is_none());
        assert_eq!(spec.system_prompt.trim(), "system prompt body");
    }
}
