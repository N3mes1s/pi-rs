//! `init_experiment`, `run_experiment`, `log_experiment` — the three tools
//! that drive an autoresearch loop. Faithful port of upstream
//! pi-autoresearch's tool surface (see
//! `davebcn87/pi-autoresearch/extensions/pi-autoresearch/index.ts`).
//!
//! Behaviour summary:
//!
//! * **`init_experiment`** writes a `{type:"config",…}` line to
//!   `<working_dir>/autoresearch.jsonl`. Re-running starts a new segment.
//! * **`run_experiment`** spawns a shell command (default 600s timeout),
//!   parses `METRIC <name>=<number>` lines from stdout, and — if
//!   `<working_dir>/autoresearch.checks.sh` exists — runs it next (default
//!   300s timeout). Returns a structured summary to the agent. Does NOT
//!   write to the JSONL log itself; that's `log_experiment`'s job.
//! * **`log_experiment`** appends a `{run:N,…}` line with `status` ∈
//!   {`keep`, `discard`, `crash`, `checks_failed`}. `keep` triggers
//!   `git add -A && git commit -m <description>`; the others trigger
//!   `git reset --hard <commit>`. The JSONL itself is preserved across
//!   reverts (it's only autoresearch.{md,sh,checks.sh,jsonl,ideas.md} that
//!   ride along with the kept-commit content; reverts back out user code
//!   only).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use pi_ai::{ToolResult, ToolSpec};
use pi_tools::{Tool, ToolContext, ToolError};
use serde_json::{json, Value};

use crate::autoresearch::log::{
    BestDirection, ConfigEntry, JsonlLog, RunEntry, RunStatus,
};

const DEFAULT_RUN_TIMEOUT_S: u64 = 600;
const DEFAULT_CHECKS_TIMEOUT_S: u64 = 300;
const RUN_OUTPUT_MAX_BYTES: usize = 4 * 1024;
const RUN_OUTPUT_MAX_LINES: usize = 10;
const CHECKS_OUTPUT_MAX_LINES: usize = 80;

// ── helpers ─────────────────────────────────────────────────────────────────

fn resolve_working_dir(ctx: &ToolContext, input: &Value) -> PathBuf {
    let s = input.get("working_dir").and_then(|v| v.as_str());
    match s {
        Some(p) => {
            let pb = PathBuf::from(p);
            if pb.is_absolute() { pb } else { ctx.cwd.join(pb) }
        }
        None => ctx.cwd.clone(),
    }
}

fn jsonl_log(working_dir: &Path) -> JsonlLog {
    JsonlLog::new(working_dir.join("autoresearch.jsonl"))
}

fn checks_path(working_dir: &Path) -> PathBuf {
    working_dir.join("autoresearch.checks.sh")
}

fn truncate_tail(s: &str, max_bytes: usize, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let kept: Vec<&str> = if lines.len() > max_lines {
        lines[lines.len() - max_lines..].to_vec()
    } else {
        lines
    };
    let body = kept.join("\n");
    if body.len() > max_bytes {
        let cut = body.len() - max_bytes;
        format!("…(truncated {cut} bytes)…\n{}", &body[cut..])
    } else {
        body
    }
}

/// Parsed METRIC lines, returning (insertion-ordered list, sorted map). The
/// primary metric is the first entry of the list (matches upstream's
/// "first METRIC line wins" rule); the map is for serialisation.
fn parse_metric_lines(stdout: &str) -> (Vec<(String, f64)>, BTreeMap<String, f64>) {
    let mut order: Vec<(String, f64)> = Vec::new();
    let mut map: BTreeMap<String, f64> = BTreeMap::new();
    let denied = ["__proto__", "constructor", "prototype"];
    for line in stdout.lines() {
        let trimmed = line.trim_end();
        let Some(rest) = trimmed.strip_prefix("METRIC ") else { continue };
        let mut parts = rest.splitn(2, '=');
        let name = match parts.next() {
            Some(n) => n.trim(),
            None => continue,
        };
        if denied.contains(&name) || name.is_empty() {
            continue;
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == 'µ')
        {
            continue;
        }
        let val_str = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };
        if let Ok(v) = val_str.parse::<f64>() {
            if v.is_finite() {
                if !map.contains_key(name) {
                    order.push((name.to_string(), v));
                }
                map.insert(name.to_string(), v);
            }
        }
    }
    (order, map)
}

async fn run_with_timeout(
    cmd: &str,
    cwd: &Path,
    deadline: Duration,
) -> std::io::Result<(String, i32, bool, bool)> {
    // Returns (output, exit_code, timed_out, crashed).
    let mut command = tokio::process::Command::new("bash");
    command
        .arg("-lc")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn()?;
    match tokio::time::timeout(deadline, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let mut buf = String::new();
            buf.push_str(&String::from_utf8_lossy(&out.stdout));
            if !out.stderr.is_empty() {
                if !buf.is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                buf.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            let code = out.status.code().unwrap_or(-1);
            let crashed = !out.status.success() && code < 0;
            Ok((buf, code, false, crashed))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Ok((String::new(), -1, true, false)),
    }
}

fn git_head_short(cwd: &Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(cwd)
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

fn git_commit_all(cwd: &Path, message: &str) -> std::io::Result<bool> {
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(cwd)
        .status()?;
    let st = std::process::Command::new("git")
        .args(["commit", "--no-gpg-sign", "-m"])
        .arg(message)
        .current_dir(cwd)
        .status()?;
    Ok(st.success())
}

fn git_reset_hard(cwd: &Path, commit: &str) -> std::io::Result<bool> {
    let st = std::process::Command::new("git")
        .args(["reset", "--hard", commit])
        .current_dir(cwd)
        .status()?;
    Ok(st.success())
}

// ── init_experiment ─────────────────────────────────────────────────────────

pub struct InitExperimentTool;

#[async_trait]
impl Tool for InitExperimentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "init_experiment".into(),
            description:
                "Configure an autoresearch session. Writes a `{type:\"config\",…}` header line to \
                 `<working_dir>/autoresearch.jsonl` (creating it if absent). Re-run to start a new \
                 segment with a different metric. Always pair with run_experiment + log_experiment."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name":         { "type": "string", "description": "Human-readable session name (e.g. 'optimise total_µs in liquid')" },
                    "metric_name":  { "type": "string", "description": "Display name for the primary metric (must match the METRIC <name>=… line emitted by autoresearch.sh)" },
                    "metric_unit":  { "type": "string", "description": "Unit. 'µs' | 'ms' | 's' | 'kb' | 'mb' | '' (default '')" },
                    "direction":    { "type": "string", "enum": ["lower", "higher"], "description": "lower or higher is better (default 'lower')" },
                    "working_dir":  { "type": "string", "description": "Where autoresearch.{md,sh,jsonl,…} live. Defaults to cwd." }
                },
                "required": ["name", "metric_name"]
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
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `name`".into()))?
            .to_string();
        let metric_name = input
            .get("metric_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `metric_name`".into()))?
            .to_string();
        let metric_unit = input
            .get("metric_unit")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let direction = match input.get("direction").and_then(|v| v.as_str()) {
            Some("higher") => BestDirection::Higher,
            _ => BestDirection::Lower,
        };
        let working_dir = resolve_working_dir(ctx, &input);
        std::fs::create_dir_all(&working_dir).map_err(ToolError::Io)?;

        let log = jsonl_log(&working_dir);
        let entry = ConfigEntry::new(name.clone(), metric_name.clone(), &metric_unit, direction);
        log.append_config(&entry).map_err(ToolError::Io)?;

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "init_experiment: name='{name}' metric='{metric_name}' unit='{metric_unit}' direction={direction:?}\nworking_dir={}\nautoresearch.jsonl ready.",
                working_dir.display()
            ),
            display: Some(json!({
                "kind": "autoresearch_init",
                "name": name,
                "metric_name": metric_name,
                "metric_unit": metric_unit,
                "direction": format!("{direction:?}").to_lowercase(),
                "working_dir": working_dir.display().to_string(),
                "jsonl_path": log.path.display().to_string(),
            })),
            is_error: false,
        })
    }
}

// ── run_experiment ──────────────────────────────────────────────────────────

pub struct RunExperimentTool;

#[async_trait]
impl Tool for RunExperimentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_experiment".into(),
            description:
                "Run a benchmark command for an autoresearch experiment. Parses METRIC <name>=<value> \
                 lines from stdout. If autoresearch.checks.sh exists, runs it after a passing \
                 benchmark. Returns a structured summary including parsed metrics, exit code, \
                 timing, and tail output. Does NOT write to autoresearch.jsonl — call \
                 log_experiment with the chosen status afterward."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command":               { "type": "string", "description": "Shell command (typically 'bash autoresearch.sh' or 'pnpm test:vitest')" },
                    "timeout_seconds":       { "type": "number", "description": "Kill after this many seconds (default 600)" },
                    "checks_timeout_seconds":{ "type": "number", "description": "Kill autoresearch.checks.sh after this many seconds (default 300)" },
                    "working_dir":           { "type": "string", "description": "Where the command runs. Defaults to cwd." }
                },
                "required": ["command"]
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
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `command`".into()))?
            .to_string();
        let timeout_s = input
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RUN_TIMEOUT_S);
        let checks_timeout_s = input
            .get("checks_timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_CHECKS_TIMEOUT_S);
        let working_dir = resolve_working_dir(ctx, &input);

        let commit = git_head_short(&working_dir);
        let t0 = Instant::now();
        let (output, exit_code, timed_out, crashed) =
            run_with_timeout(&command, &working_dir, Duration::from_secs(timeout_s))
                .await
                .map_err(ToolError::Io)?;
        let duration_seconds = t0.elapsed().as_secs_f64();
        let (ordered, metrics) = parse_metric_lines(&output);
        let parsed_primary = ordered.first().map(|(_, v)| *v);

        // After a passing benchmark, run checks.sh if present.
        let mut checks_pass: Option<bool> = None;
        let mut checks_output = String::new();
        let mut checks_duration = 0.0;
        let mut checks_timed_out = false;
        let passed = !timed_out && !crashed && exit_code == 0;
        let checks_path = checks_path(&working_dir);
        if passed && checks_path.exists() {
            let cmd = format!("bash {}", shell_escape(&checks_path.display().to_string()));
            let t1 = Instant::now();
            let (out, code, t_out, _crash) =
                run_with_timeout(&cmd, &working_dir, Duration::from_secs(checks_timeout_s))
                    .await
                    .map_err(ToolError::Io)?;
            checks_duration = t1.elapsed().as_secs_f64();
            checks_timed_out = t_out;
            checks_output = truncate_tail(&out, RUN_OUTPUT_MAX_BYTES, CHECKS_OUTPUT_MAX_LINES);
            checks_pass = Some(!t_out && code == 0);
        }

        let tail = truncate_tail(&output, RUN_OUTPUT_MAX_BYTES, RUN_OUTPUT_MAX_LINES);

        let mut summary = format!(
            "run_experiment: exit={exit_code} duration={:.1}s passed={passed} timed_out={timed_out} crashed={crashed}\ncommit_before={commit}\n",
            duration_seconds
        );
        if !metrics.is_empty() {
            summary.push_str("metrics:\n");
            for (k, v) in &metrics {
                summary.push_str(&format!("  {k}={v}\n"));
            }
        } else {
            summary.push_str("metrics: (no METRIC lines parsed)\n");
        }
        if let Some(cp) = checks_pass {
            summary.push_str(&format!(
                "checks: pass={cp} duration={:.1}s timed_out={checks_timed_out}\n",
                checks_duration
            ));
            if !cp {
                summary.push_str("checks_output (last 80 lines):\n");
                summary.push_str(&checks_output);
                summary.push('\n');
            }
        }
        summary.push_str("output (tail):\n");
        summary.push_str(&tail);

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: summary,
            display: Some(json!({
                "kind": "autoresearch_run",
                "command": command,
                "commit_before": commit,
                "exit_code": exit_code,
                "duration_seconds": duration_seconds,
                "passed": passed,
                "timed_out": timed_out,
                "crashed": crashed,
                "metrics": metrics,
                "primary_metric": parsed_primary,
                "checks_pass": checks_pass,
                "checks_timed_out": checks_timed_out,
                "checks_duration": checks_duration,
                "working_dir": working_dir.display().to_string(),
            })),
            is_error: !passed && !crashed && !timed_out,
        })
    }
}

fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '=' | ':'))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

// ── log_experiment ──────────────────────────────────────────────────────────

pub struct LogExperimentTool;

#[async_trait]
impl Tool for LogExperimentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "log_experiment".into(),
            description:
                "Record an experiment outcome. Appends a `{run:N,…}` line to \
                 autoresearch.jsonl. status='keep' triggers `git add -A && git commit -m \
                 description`; the others trigger `git reset --hard commit`. ASI is the \
                 free-form note dict that survives revert (the only memory of a discarded run \
                 the next agent will see)."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "commit":         { "type": "string", "description": "Short git commit hash captured before the run (from run_experiment.commit_before)" },
                    "metric":         { "type": "number", "description": "Primary metric value. 0 for crashes/timeouts." },
                    "status":         { "type": "string", "enum": ["keep", "discard", "crash", "checks_failed"], "description": "keep auto-commits; the others auto-revert." },
                    "description":    { "type": "string", "description": "One-line description of what this experiment tried." },
                    "metrics":        { "type": "object", "description": "Secondary metrics dict {name: number}." },
                    "asi":            { "type": "object", "description": "Free-form key/value annotations the agent wants to remember." },
                    "iteration_tokens":{ "type": "integer", "description": "Tokens consumed during this iteration (optional)." },
                    "confidence":     { "type": "number", "description": "Optional confidence score (best_improvement / MAD)." },
                    "working_dir":    { "type": "string" }
                },
                "required": ["commit", "metric", "status", "description"]
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
        let commit = input
            .get("commit")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `commit`".into()))?
            .to_string();
        let metric = input
            .get("metric")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| ToolError::InvalidInput("missing `metric` (number)".into()))?;
        let status_str = input
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `status`".into()))?;
        let status = RunStatus::parse(status_str)
            .ok_or_else(|| ToolError::InvalidInput(format!("bad status: {status_str}")))?;
        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `description`".into()))?
            .to_string();
        let metrics_in = input.get("metrics").cloned().unwrap_or_else(|| json!({}));
        let mut metrics: BTreeMap<String, f64> = BTreeMap::new();
        if let Some(obj) = metrics_in.as_object() {
            for (k, v) in obj {
                if let Some(n) = v.as_f64() {
                    metrics.insert(k.clone(), n);
                }
            }
        }
        let asi = input.get("asi").cloned();
        let iteration_tokens = input.get("iteration_tokens").and_then(|v| v.as_u64());
        let confidence = input.get("confidence").and_then(|v| v.as_f64());
        let working_dir = resolve_working_dir(ctx, &input);

        let log = jsonl_log(&working_dir);
        let run_number = log.next_run_number().map_err(ToolError::Io)?;

        let entry = RunEntry {
            run: run_number,
            commit: commit.clone(),
            metric,
            metrics,
            status,
            description: description.clone(),
            timestamp: Utc::now().timestamp_millis(),
            confidence,
            iteration_tokens,
            asi,
        };
        log.append_run(&entry).map_err(ToolError::Io)?;

        // Git side-effects.
        let mut git_msg = String::new();
        match status {
            RunStatus::Keep => {
                let ok = git_commit_all(&working_dir, &description).map_err(ToolError::Io)?;
                git_msg = if ok {
                    format!("git committed: {description}")
                } else {
                    "git commit failed (no changes? check working_dir)".into()
                };
            }
            RunStatus::Discard | RunStatus::Crash | RunStatus::ChecksFailed => {
                if !commit.is_empty() {
                    let ok =
                        git_reset_hard(&working_dir, &commit).map_err(ToolError::Io)?;
                    git_msg = if ok {
                        format!("git reset --hard {commit}")
                    } else {
                        format!("git reset --hard {commit} FAILED")
                    };
                }
            }
        }

        Ok(ToolResult {
            tool_use_id: call_id.into(),
            model_output: format!(
                "log_experiment: run={run_number} status={status_str} metric={metric} description='{description}'\n{git_msg}",
            ),
            display: Some(json!({
                "kind": "autoresearch_log",
                "run": run_number,
                "status": status_str,
                "metric": metric,
                "commit": commit,
                "git_action": git_msg,
                "working_dir": working_dir.display().to_string(),
            })),
            is_error: false,
        })
    }
}

// ── helper for tests ────────────────────────────────────────────────────────

/// Parse a single METRIC line. Public for tests + callers that need the same
/// regex behaviour as run_experiment.
pub fn parse_metric(output: &str, metric_name: &str) -> Option<f64> {
    parse_metric_lines(output).1.get(metric_name).copied()
}
