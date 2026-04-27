//! Autoresearch [`pi_tools::Tool`] implementations.
//!
//! Three tools are exposed to the agent:
//!
//! | Tool | Purpose |
//! |------|---------|
//! | [`InitExperimentTool`] | Bootstrap a new experiment (writes config + md, logs Init entry) |
//! | [`RunExperimentTool`]  | Run a benchmark command and parse `METRIC name=value` from stdout |
//! | [`LogExperimentTool`]  | Record the result, keep or revert, commit or reset HEAD |

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde_json::{json, Value};

use crate::autoresearch::{
    log::{JsonlLog, LogEntryKind},
    session::{MetricDirection, Session, SessionConfig},
};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Parse lines of the form `METRIC <name>=<number>` from `output`.
/// Returns the first matching value for `metric_name`, or `None`.
pub fn parse_metric(output: &str, metric_name: &str) -> Option<f64> {
    let prefix = format!("METRIC {}=", metric_name);
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            if let Ok(v) = rest.trim().parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

/// Run a shell command synchronously (blocking), returning (stdout, exit_ok).
fn run_shell(cmd: &str, cwd: &std::path::Path) -> std::io::Result<(String, bool)> {
    let output = std::process::Command::new("bash")
        .args(["-lc", cmd])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str("[stderr]\n");
        combined.push_str(&stderr);
    }
    Ok((combined, output.status.success()))
}

/// Return the current git HEAD SHA (short) or an empty string on failure.
fn git_head(cwd: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Resolve the working directory for git commands: prefer `config.working_dir`,
/// fall back to `root`.
fn work_dir(root: &std::path::Path, config: &SessionConfig) -> PathBuf {
    config
        .working_dir
        .clone()
        .unwrap_or_else(|| root.to_path_buf())
}

// ── InitExperimentTool ────────────────────────────────────────────────────────

/// Initialise a new autoresearch experiment.
///
/// **Input JSON fields**
///
/// | Field | Type | Required | Description |
/// |-------|------|----------|-------------|
/// | `root` | string | yes | Directory where artefact files will be written |
/// | `name` | string | yes | Human-readable experiment name |
/// | `metric` | string | yes | Name of the metric (matches `METRIC <name>=…` lines) |
/// | `unit` | string | yes | Display unit (e.g. `"ms"`) |
/// | `direction` | `"lower"` \| `"higher"` | yes | Whether lower or higher values are improvements |
/// | `max_iterations` | integer | no | Hard iteration cap (`null` = unlimited) |
/// | `working_dir` | string | no | Git working directory (defaults to `root`) |
pub struct InitExperimentTool;

#[async_trait]
impl Tool for InitExperimentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "autoresearch_init".into(),
            description:
                "Initialise a new autoresearch experiment. Writes autoresearch.config.json \
                 and autoresearch.md, and appends an Init log entry to autoresearch.jsonl."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "root":           { "type": "string" },
                    "name":           { "type": "string" },
                    "metric":         { "type": "string" },
                    "unit":           { "type": "string" },
                    "direction":      { "type": "string", "enum": ["lower", "higher"] },
                    "max_iterations": { "type": "integer" },
                    "working_dir":    { "type": "string" }
                },
                "required": ["root", "name", "metric", "unit", "direction"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        // ── parse inputs ────────────────────────────────────────────────────
        let root_str = input
            .get("root")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `root`".into()))?;
        let root: PathBuf = {
            let p = std::path::Path::new(root_str);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                ctx.cwd.join(p)
            }
        };

        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `name`".into()))?
            .to_string();
        let metric = input
            .get("metric")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `metric`".into()))?
            .to_string();
        let unit = input
            .get("unit")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `unit`".into()))?
            .to_string();
        let direction = match input
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("lower")
        {
            "higher" => MetricDirection::Higher,
            _ => MetricDirection::Lower,
        };
        let max_iterations = input
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let working_dir = input
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|p| {
                let pb = std::path::Path::new(p);
                if pb.is_absolute() {
                    pb.to_path_buf()
                } else {
                    ctx.cwd.join(pb)
                }
            });

        // ── create root dir if needed ───────────────────────────────────────
        std::fs::create_dir_all(&root)?;

        let config = SessionConfig {
            name: name.clone(),
            metric: metric.clone(),
            unit: unit.clone(),
            direction,
            max_iterations,
            working_dir,
        };
        let session = Session::new(&root, config.clone());

        // ── persist config + markdown ───────────────────────────────────────
        session.save_config()?;
        session.save_md()?;

        // ── append Init log entry ───────────────────────────────────────────
        let log = JsonlLog::new(session.jsonl_path(), direction);
        let entry = log.append(LogEntryKind::Init { config })?;

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "Experiment '{}' initialised.\n\
                 config: {}\n\
                 log:    {}\n\
                 md:     {}\n\
                 entry id: {}",
                name,
                session.config_path().display(),
                session.jsonl_path().display(),
                session.md_path().display(),
                entry.id,
            ),
            display: Some(json!({
                "kind": "autoresearch_init",
                "name": name,
                "metric": metric,
                "root": root.display().to_string(),
                "entry_id": entry.id,
            })),
            is_error: false,
        })
    }
}

// ── RunExperimentTool ─────────────────────────────────────────────────────────

/// Run a benchmark command and parse the metric value from its stdout.
///
/// **Input JSON fields**
///
/// | Field | Type | Required | Description |
/// |-------|------|----------|-------------|
/// | `root` | string | yes | Experiment root (to locate `autoresearch.config.json`) |
/// | `command` | string | yes | Shell command to execute via `bash -lc` |
/// | `idea` | string | yes | One-line description of the change being benchmarked |
///
/// The tool records `git rev-parse --short HEAD` as `commit_before`,
/// executes `command`, and scans stdout for a line matching
/// `METRIC <metric_name>=<number>`.  Returns the parsed value (or an error
/// if none is found).  A [`LogEntryKind::Run`] entry is appended.
pub struct RunExperimentTool;

#[async_trait]
impl Tool for RunExperimentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "autoresearch_run".into(),
            description:
                "Run a benchmark command for an autoresearch experiment. \
                 Parses `METRIC <name>=<value>` from stdout and appends a Run log entry."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "root":    { "type": "string" },
                    "command": { "type": "string" },
                    "idea":    { "type": "string" }
                },
                "required": ["root", "command", "idea"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let root = resolve_root(ctx, &input)?;
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `command`".into()))?
            .to_string();
        let idea = input
            .get("idea")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `idea`".into()))?
            .to_string();

        let session = Session::load(&root)
            .map_err(|e| ToolError::Other(format!("cannot load session: {}", e)))?;
        let cwd = work_dir(&root, &session.config);

        // Record HEAD before the run.
        let commit_before = git_head(&cwd);

        // Run the benchmark command.
        let t0 = Instant::now();
        let (output, _ok) =
            run_shell(&command, &cwd).map_err(ToolError::Io)?;
        let duration_ms = t0.elapsed().as_millis() as u64;

        // Parse metric.
        let metric_value = parse_metric(&output, &session.config.metric);

        // Append Run log entry.
        let log = JsonlLog::new(session.jsonl_path(), session.config.direction);
        let entry = log
            .append(LogEntryKind::Run {
                idea: idea.clone(),
                commit_before: commit_before.clone(),
            })
            .map_err(ToolError::Io)?;

        let (model_output, is_error) = match metric_value {
            Some(v) => (
                format!(
                    "Run logged. idea: {}\ncommit_before: {}\n\
                     METRIC {}={}\nduration: {}ms\nrun_id: {}",
                    idea, commit_before, session.config.metric, v, duration_ms, entry.id
                ),
                false,
            ),
            None => (
                format!(
                    "Run logged but no METRIC line found for '{}' in output.\n\
                     idea: {}\ncommit_before: {}\nduration: {}ms\nrun_id: {}\n\
                     --- output ---\n{}",
                    session.config.metric, idea, commit_before, duration_ms, entry.id, output
                ),
                true,
            ),
        };

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output,
            display: Some(json!({
                "kind": "autoresearch_run",
                "idea": idea,
                "run_id": entry.id,
                "commit_before": commit_before,
                "metric_value": metric_value,
                "duration_ms": duration_ms,
            })),
            is_error,
        })
    }
}

// ── LogExperimentTool ─────────────────────────────────────────────────────────

/// Record the final result of an experiment run.
///
/// **Input JSON fields**
///
/// | Field | Type | Required | Description |
/// |-------|------|----------|-------------|
/// | `root` | string | yes | Experiment root |
/// | `run_id` | string | yes | `id` of the corresponding `Run` log entry |
/// | `metric_value` | number | yes | The observed metric value |
/// | `kept` | bool | yes | Whether to keep (`true`) or revert (`false`) the change |
/// | `commit_before` | string | yes | Git SHA recorded before the run (used for revert) |
/// | `idea` | string | yes | Idea description (used in commit message when keeping) |
/// | `duration_ms` | integer | no | Wall-clock duration (default 0) |
///
/// When `kept=true` the tool runs `git add -A && git commit -m "autoresearch: <idea>"`.
/// When `kept=false` the tool runs `git reset --hard <commit_before>`.
///
/// If `autoresearch.checks.sh` exists and is executable it is run first;
/// `checks_passed` in the log entry reflects its exit code.
pub struct LogExperimentTool;

#[async_trait]
impl Tool for LogExperimentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "autoresearch_log".into(),
            description:
                "Record the result of an autoresearch run. Commits the change when kept=true, \
                 reverts to commit_before when kept=false. Appends a Result log entry."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "root":          { "type": "string" },
                    "run_id":        { "type": "string" },
                    "metric_value":  { "type": "number" },
                    "kept":          { "type": "boolean" },
                    "commit_before": { "type": "string" },
                    "idea":          { "type": "string" },
                    "duration_ms":   { "type": "integer" }
                },
                "required": ["root", "run_id", "metric_value", "kept", "commit_before", "idea"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    async fn invoke(
        &self,
        ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let root = resolve_root(ctx, &input)?;
        let run_id = input
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `run_id`".into()))?
            .to_string();
        let metric_value = input
            .get("metric_value")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| ToolError::InvalidInput("missing `metric_value`".into()))?;
        let kept = input
            .get("kept")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| ToolError::InvalidInput("missing `kept`".into()))?;
        let commit_before = input
            .get("commit_before")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `commit_before`".into()))?
            .to_string();
        let idea = input
            .get("idea")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `idea`".into()))?
            .to_string();
        let duration_ms = input
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let session = Session::load(&root)
            .map_err(|e| ToolError::Other(format!("cannot load session: {}", e)))?;
        let cwd = work_dir(&root, &session.config);

        // ── run checks script (if present) ──────────────────────────────────
        let checks_script = session.checks_script();
        let checks_passed = if checks_script.exists() {
            let (_, ok) = run_shell(
                &checks_script.display().to_string(),
                &cwd,
            )
            .unwrap_or_default();
            ok
        } else {
            true // no checks script → assume passing
        };

        // ── git operations ──────────────────────────────────────────────────
        let git_output = if kept {
            // Stage everything and commit.
            let add_cmd = "git add -A";
            let commit_cmd = format!(r#"git commit -m "autoresearch: {}""#, idea.replace('"', "'"));
            let (add_out, _) = run_shell(add_cmd, &cwd).unwrap_or_default();
            let (commit_out, _) = run_shell(&commit_cmd, &cwd).unwrap_or_default();
            format!("{}\n{}", add_out, commit_out)
        } else {
            // Hard-reset to the pre-run commit.
            let reset_cmd = format!("git reset --hard {}", commit_before);
            let (out, _) = run_shell(&reset_cmd, &cwd).unwrap_or_default();
            out
        };

        let commit_after = git_head(&cwd);

        // ── append Result log entry ─────────────────────────────────────────
        let log = JsonlLog::new(session.jsonl_path(), session.config.direction);
        let entry = log
            .append(LogEntryKind::Result {
                run_id: run_id.clone(),
                metric_value,
                duration_ms,
                kept,
                commit_after: commit_after.clone(),
                checks_passed,
            })
            .map_err(ToolError::Io)?;

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "Result recorded.\n\
                 run_id: {}\nmetric_value: {}\nkept: {}\n\
                 checks_passed: {}\ncommit_after: {}\n\
                 entry_id: {}\n--- git ---\n{}",
                run_id,
                metric_value,
                kept,
                checks_passed,
                commit_after,
                entry.id,
                git_output.trim(),
            ),
            display: Some(json!({
                "kind": "autoresearch_log",
                "run_id": run_id,
                "metric_value": metric_value,
                "kept": kept,
                "checks_passed": checks_passed,
                "commit_after": commit_after,
                "entry_id": entry.id,
            })),
            is_error: false,
        })
    }
}

// ── shared helpers ────────────────────────────────────────────────────────────

fn resolve_root(ctx: &ToolContext, input: &Value) -> Result<PathBuf, ToolError> {
    let root_str = input
        .get("root")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing `root`".into()))?;
    let p = std::path::Path::new(root_str);
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.cwd.join(p)
    })
}
