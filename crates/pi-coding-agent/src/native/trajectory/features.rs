//! Feature extraction over a session branch.
//!
//! These are deterministic signals — *evidence* — that get folded into the
//! judge's prompt alongside the raw transcript. The judge (G2,
//! `judge::judge_session`) is what makes the actual win/loss call;
//! features just give it pre-condensed anchors so it doesn't have to
//! re-derive "did the test suite pass" from a 10 KB log line.
//!
//! Signals extracted:
//!
//! - `test_runs`   — every test-runner bash invocation + exit code
//! - `compile_runs`— every compile / type-check invocation + exit code
//! - `edit_errors` — write/edit tool calls that returned `is_error`
//! - `repeated_reads` — same read/grep target hit 3+× (potential loop)
//! - `last_termination` — whether the session ended on an unrecovered error
//!
//! No interpretation, no scoring — just structured evidence.

use pi_agent_core::{SessionEntry, SessionEntryKind};
use pi_ai::ToolCall;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct TrajectoryFeatures {
    pub test_runs: Vec<CommandRun>,
    pub compile_runs: Vec<CommandRun>,
    pub edit_errors: Vec<EditError>,
    pub repeated_reads: Vec<RepeatedRead>,
    pub last_termination: Termination,
    pub turn_counts: TurnCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandRun {
    pub command: String,
    pub exit: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditError {
    pub tool: String,
    pub path: Option<String>,
    pub message: String,
    pub recovered: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepeatedRead {
    pub target: String,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    /// Final entry was an assistant message (clean turn).
    Clean,
    /// Final tool result was an error not followed by a recovery.
    Error,
    /// Branch is empty / no signal.
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TurnCounts {
    pub user: u32,
    pub assistant: u32,
    pub tool_calls: u32,
    pub tool_results: u32,
    pub tool_errors: u32,
}

/// Walk a session branch once and produce all features.
pub fn extract(branch: &[SessionEntry]) -> TrajectoryFeatures {
    let calls = collect_tool_calls(branch);
    let mut test_runs = Vec::new();
    let mut compile_runs = Vec::new();
    let mut edit_errors_map: HashMap<String, EditError> = HashMap::new();
    let mut read_counts: HashMap<(String, String), u32> = HashMap::new();
    let mut turn_counts = TurnCounts::default();

    for e in branch {
        match &e.kind {
            SessionEntryKind::User { .. } => turn_counts.user += 1,
            SessionEntryKind::Assistant { .. } => turn_counts.assistant += 1,
            SessionEntryKind::ToolCall { call } => {
                turn_counts.tool_calls += 1;
                if is_read_like_tool(&call.name) {
                    let key = (call.name.clone(), canonicalise_input(call));
                    *read_counts.entry(key).or_insert(0) += 1;
                }
            }
            SessionEntryKind::ToolResult { result } => {
                turn_counts.tool_results += 1;
                if result.is_error {
                    turn_counts.tool_errors += 1;
                }
                let Some(call) = calls.get(&result.tool_use_id) else {
                    continue;
                };
                if call.name == "bash" {
                    if let Some(cmd) = call.input.get("command").and_then(|v| v.as_str()) {
                        let exit = exit_code(result);
                        if is_test_command(cmd) {
                            test_runs.push(CommandRun {
                                command: short(cmd, 80),
                                exit,
                            });
                        } else if is_compile_command(cmd) {
                            compile_runs.push(CommandRun {
                                command: short(cmd, 80),
                                exit,
                            });
                        }
                    }
                } else if is_write_like_tool(&call.name) {
                    let path_key = call
                        .input
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if result.is_error {
                        edit_errors_map.insert(
                            path_key,
                            EditError {
                                tool: call.name.clone(),
                                path: call
                                    .input
                                    .get("path")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                message: short(&result.model_output, 200),
                                recovered: false,
                            },
                        );
                    } else if let Some(err) = edit_errors_map.get_mut(&path_key) {
                        err.recovered = true;
                    }
                }
            }
            _ => {}
        }
    }

    let edit_errors: Vec<EditError> = edit_errors_map.into_values().collect();

    let repeated_reads: Vec<RepeatedRead> = read_counts
        .into_iter()
        .filter(|(_, n)| *n >= 3)
        .map(|((tool, target), count)| RepeatedRead {
            target: format!("{tool}:{target}"),
            count,
        })
        .collect();

    let last_termination = termination_state(branch);

    TrajectoryFeatures {
        test_runs,
        compile_runs,
        edit_errors,
        repeated_reads,
        last_termination,
        turn_counts,
    }
}

// ─── helpers ────────────────────────────────────────────────────────────

fn collect_tool_calls(branch: &[SessionEntry]) -> HashMap<String, ToolCall> {
    let mut by_id = HashMap::new();
    for e in branch {
        if let SessionEntryKind::ToolCall { call } = &e.kind {
            by_id.insert(call.id.clone(), call.clone());
        }
    }
    by_id
}

fn exit_code(result: &pi_ai::ToolResult) -> i64 {
    result
        .display
        .as_ref()
        .and_then(|d| d.get("exit"))
        .and_then(|v| v.as_i64())
        .or_else(|| parse_trailing_exit(&result.model_output))
        .unwrap_or(if result.is_error { -1 } else { 0 })
}

fn parse_trailing_exit(output: &str) -> Option<i64> {
    let needle = "[exit ";
    let i = output.rfind(needle)?;
    let rest = &output[i + needle.len()..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

fn termination_state(branch: &[SessionEntry]) -> Termination {
    if branch.is_empty() {
        return Termination::Unknown;
    }
    for e in branch.iter().rev() {
        match &e.kind {
            SessionEntryKind::Assistant { .. } => return Termination::Clean,
            SessionEntryKind::ToolResult { result } => {
                return if result.is_error {
                    Termination::Error
                } else {
                    Termination::Clean
                };
            }
            _ => continue,
        }
    }
    Termination::Unknown
}

fn is_test_command(cmd: &str) -> bool {
    let lc = cmd.to_lowercase();
    [
        "cargo test", "cargo nextest", "cargo bench",
        "pytest", "python -m pytest", "python -m unittest",
        "npm test", "npm run test", "yarn test", "pnpm test",
        "npx jest", "npx vitest", "jest", "vitest",
        "go test",
        "mvn test", "gradle test",
        "bundle exec rspec", "rake test", "rspec",
        "mix test", "phpunit",
    ]
    .iter()
    .any(|p| lc.contains(p))
}

fn is_compile_command(cmd: &str) -> bool {
    let lc = cmd.to_lowercase();
    [
        "cargo build", "cargo check", "cargo clippy",
        "tsc", "tsc --noemit",
        "mypy", "ruff check", "pyright",
        "npm run build", "yarn build", "pnpm build",
        "make", "cmake --build",
        "go build", "go vet",
        "javac", "mvn compile",
    ]
    .iter()
    .any(|p| lc.contains(p))
}

fn is_read_like_tool(name: &str) -> bool {
    matches!(name, "read" | "grep" | "find" | "ls")
}

fn is_write_like_tool(name: &str) -> bool {
    matches!(name, "edit" | "write")
}

fn canonicalise_input(call: &ToolCall) -> String {
    match call.name.as_str() {
        "read" => call
            .input
            .get("file_path")
            .or_else(|| call.input.get("path"))
            .map(|v| v.to_string())
            .unwrap_or_default(),
        "grep" => format!(
            "{}|{}",
            call.input.get("pattern").map(|v| v.to_string()).unwrap_or_default(),
            call.input.get("path").map(|v| v.to_string()).unwrap_or_default(),
        ),
        "find" => call.input.get("path").map(|v| v.to_string()).unwrap_or_default(),
        "ls" => call.input.get("path").map(|v| v.to_string()).unwrap_or_default(),
        _ => call.input.to_string(),
    }
}

fn short(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let truncated: String = trimmed.chars().take(max).collect();
        format!("{}…", truncated)
    }
}
