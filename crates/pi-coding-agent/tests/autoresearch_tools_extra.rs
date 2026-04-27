//! Extra coverage for autoresearch tools.
//!
//! Covers:
//! - InitExperimentTool::spec() field values
//! - InitExperimentTool: missing required fields → ToolError
//! - InitExperimentTool: relative root resolved against cwd
//! - InitExperimentTool: working_dir field (relative and absolute)
//! - RunExperimentTool::spec() field values
//! - RunExperimentTool: missing required fields → ToolError
//! - RunExperimentTool: no METRIC line → is_error=true
//! - LogExperimentTool::spec() field values
//! - LogExperimentTool::read_only() is false for all tools
//! - LogExperimentTool: kept=false reverts (git reset --hard) in a real temp git repo

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    log::{JsonlLog, LogEntryKind},
    session::MetricDirection,
    tools::{InitExperimentTool, LogExperimentTool, RunExperimentTool},
};
use pi_tools::{Tool, ToolContext};

// ── helpers ───────────────────────────────────────────────────────────────────

fn ctx(dir: &TempDir) -> ToolContext {
    ToolContext {
        cwd: dir.path().to_path_buf(),
        ..Default::default()
    }
}

/// Initialise a bare git repo in `dir` with a single commit so that
/// `git rev-parse HEAD` works and `git reset --hard` can be tested.
#[cfg(unix)]
fn git_init_with_commit(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed")
    };
    run(&["init"]);
    run(&["config", "user.email", "test@test.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "commit.gpgsign", "false"]);
    // Create a file and commit it so HEAD exists.
    fs::write(dir.join("README.md"), b"init").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
}

// ── InitExperimentTool::spec() ────────────────────────────────────────────────

#[test]
fn init_tool_spec_has_correct_name() {
    let spec = InitExperimentTool.spec();
    assert_eq!(spec.name, "autoresearch_init");
}

#[test]
fn init_tool_spec_description_non_empty() {
    let spec = InitExperimentTool.spec();
    assert!(!spec.description.is_empty());
}

#[test]
fn init_tool_read_only_is_false() {
    assert!(!InitExperimentTool.read_only());
}

// ── InitExperimentTool: missing required fields → ToolError ───────────────────

#[test]
fn init_tool_missing_root_gives_error() {
    let dir = TempDir::new().unwrap();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"name": "x", "metric": "m", "unit": "u", "direction": "lower"}),
    ));
    assert!(result.is_err(), "missing root should give ToolError");
}

#[test]
fn init_tool_missing_name_gives_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "metric": "m", "unit": "u", "direction": "lower"}),
    ));
    assert!(result.is_err());
}

#[test]
fn init_tool_missing_metric_gives_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "name": "n", "unit": "u", "direction": "lower"}),
    ));
    assert!(result.is_err());
}

#[test]
fn init_tool_missing_unit_gives_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "name": "n", "metric": "m", "direction": "lower"}),
    ));
    assert!(result.is_err());
}

// ── InitExperimentTool: direction variants ─────────────────────────────────────

#[test]
fn init_tool_higher_direction() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "name": "x", "metric": "fps", "unit": "f/s", "direction": "higher"}),
    ))
    .unwrap();
    assert!(!result.is_error);
    // Verify config was written with Higher direction.
    let session = pi_coding_agent::autoresearch::session::Session::load(dir.path()).unwrap();
    assert_eq!(session.config.direction, MetricDirection::Higher);
}

#[test]
fn init_tool_unknown_direction_defaults_to_lower() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "name": "x", "metric": "m", "unit": "u", "direction": "bogus"}),
    ))
    .unwrap();
    assert!(!result.is_error);
    let session = pi_coding_agent::autoresearch::session::Session::load(dir.path()).unwrap();
    assert_eq!(session.config.direction, MetricDirection::Lower);
}

// ── InitExperimentTool: max_iterations field ──────────────────────────────────

#[test]
fn init_tool_max_iterations_set() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "name": "x", "metric": "m", "unit": "u", "direction": "lower", "max_iterations": 5}),
    ))
    .unwrap();
    assert!(!result.is_error);
    let session = pi_coding_agent::autoresearch::session::Session::load(dir.path()).unwrap();
    assert_eq!(session.config.max_iterations, Some(5));
}

// ── InitExperimentTool: relative root resolved against cwd ────────────────────

#[test]
fn init_tool_relative_root_resolved_against_cwd() {
    let dir = TempDir::new().unwrap();
    // Use a relative path "subdir" — should be resolved as <dir>/subdir.
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": "subdir", "name": "x", "metric": "m", "unit": "u", "direction": "lower"}),
    ))
    .unwrap();
    assert!(!result.is_error);
    assert!(dir.path().join("subdir").join("autoresearch.config.json").exists());
}

// ── InitExperimentTool: absolute working_dir field ────────────────────────────

#[test]
fn init_tool_absolute_working_dir() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let wd = dir.path().to_str().unwrap().to_string();
    let result = tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": root, "name": "x", "metric": "m", "unit": "u", "direction": "lower", "working_dir": wd}),
    ))
    .unwrap();
    assert!(!result.is_error);
    let session = pi_coding_agent::autoresearch::session::Session::load(dir.path()).unwrap();
    assert!(session.config.working_dir.is_some());
}

// ── RunExperimentTool::spec() ─────────────────────────────────────────────────

#[test]
fn run_tool_spec_has_correct_name() {
    let spec = RunExperimentTool.spec();
    assert_eq!(spec.name, "autoresearch_run");
}

#[test]
fn run_tool_spec_description_non_empty() {
    let spec = RunExperimentTool.spec();
    assert!(!spec.description.is_empty());
}

#[test]
fn run_tool_read_only_is_false() {
    assert!(!RunExperimentTool.read_only());
}

// ── RunExperimentTool: missing required fields → ToolError ────────────────────

#[test]
fn run_tool_missing_root_gives_error() {
    let dir = TempDir::new().unwrap();
    let result = tokio_test::block_on(RunExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"command": "echo hi", "idea": "test"}),
    ));
    assert!(result.is_err());
}

#[test]
fn run_tool_missing_command_gives_error() {
    let dir = TempDir::new().unwrap();
    // First create a session.
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": dir.path().to_str().unwrap(), "name": "x", "metric": "m", "unit": "u", "direction": "lower"}),
    ))
    .unwrap();
    let result = tokio_test::block_on(RunExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": dir.path().to_str().unwrap(), "idea": "test"}),
    ));
    assert!(result.is_err());
}

#[test]
fn run_tool_missing_idea_gives_error() {
    let dir = TempDir::new().unwrap();
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": dir.path().to_str().unwrap(), "name": "x", "metric": "m", "unit": "u", "direction": "lower"}),
    ))
    .unwrap();
    let result = tokio_test::block_on(RunExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": dir.path().to_str().unwrap(), "command": "echo hi"}),
    ));
    assert!(result.is_err());
}

// ── RunExperimentTool: session not found → ToolError::Other ──────────────────

#[test]
fn run_tool_no_session_gives_error() {
    let dir = TempDir::new().unwrap();
    let result = tokio_test::block_on(RunExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"root": dir.path().to_str().unwrap(), "command": "echo hi", "idea": "test"}),
    ));
    assert!(result.is_err());
}

// ── RunExperimentTool: no METRIC line → is_error=true ────────────────────────

#[test]
fn run_tool_no_metric_line_is_error() {
    let dir = TempDir::new().unwrap();
    // Init experiment looking for metric "latency".
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "name": "no-metric",
            "metric": "latency",
            "unit": "ms",
            "direction": "lower"
        }),
    ))
    .unwrap();

    // Run a command that produces no METRIC line.
    let result = tokio_test::block_on(RunExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "command": "echo 'no metric here'",
            "idea": "test-no-metric"
        }),
    ))
    .unwrap();

    assert!(result.is_error, "missing METRIC line should give is_error=true");
    assert!(
        result.model_output.contains("no METRIC line"),
        "output should mention missing metric; got: {}",
        result.model_output
    );
}

// ── RunExperimentTool: METRIC line found → is_error=false ────────────────────

#[test]
fn run_tool_metric_line_found_is_not_error() {
    let dir = TempDir::new().unwrap();
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "name": "with-metric",
            "metric": "latency",
            "unit": "ms",
            "direction": "lower"
        }),
    ))
    .unwrap();

    let result = tokio_test::block_on(RunExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "command": "echo 'METRIC latency=42.5'",
            "idea": "test-with-metric"
        }),
    ))
    .unwrap();

    assert!(!result.is_error, "METRIC line present should not be error; got: {}", result.model_output);
    assert!(result.model_output.contains("42.5"));
}

// ── LogExperimentTool::spec() ─────────────────────────────────────────────────

#[test]
fn log_tool_spec_has_correct_name() {
    let spec = LogExperimentTool.spec();
    assert_eq!(spec.name, "autoresearch_log");
}

#[test]
fn log_tool_spec_description_non_empty() {
    let spec = LogExperimentTool.spec();
    assert!(!spec.description.is_empty());
}

#[test]
fn log_tool_read_only_is_false() {
    assert!(!LogExperimentTool.read_only());
}

// ── LogExperimentTool: missing required fields → ToolError ────────────────────

#[test]
fn log_tool_missing_root_gives_error() {
    let dir = TempDir::new().unwrap();
    let result = tokio_test::block_on(LogExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({"run_id": "r1", "metric_value": 1.0, "kept": true, "commit_before": "abc", "idea": "x"}),
    ));
    assert!(result.is_err());
}

#[test]
fn log_tool_missing_run_id_gives_error() {
    let dir = TempDir::new().unwrap();
    // Init a session first.
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "name": "x", "metric": "m", "unit": "u", "direction": "lower"
        }),
    ))
    .unwrap();
    let result = tokio_test::block_on(LogExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "metric_value": 1.0, "kept": true, "commit_before": "abc", "idea": "x"
        }),
    ));
    assert!(result.is_err());
}

// ── LogExperimentTool: no session → ToolError::Other ─────────────────────────

#[test]
fn log_tool_no_session_gives_error() {
    let dir = TempDir::new().unwrap();
    let result = tokio_test::block_on(LogExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "run_id": "r1", "metric_value": 1.0, "kept": true, "commit_before": "abc", "idea": "x"
        }),
    ));
    assert!(result.is_err());
}

// ── LogExperimentTool: kept=false runs git reset --hard ───────────────────────

#[cfg(unix)]
#[test]
fn log_tool_kept_false_reverts_changes() {
    let dir = TempDir::new().unwrap();
    // Set up a git repo.
    git_init_with_commit(dir.path());

    // Init autoresearch session.
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "name": "revert-test",
            "metric": "latency",
            "unit": "ms",
            "direction": "lower",
            "working_dir": dir.path().to_str().unwrap()
        }),
    ))
    .unwrap();

    // Get the current HEAD.
    let head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // If git is unavailable or commit failed, skip.
    if head.is_empty() {
        return;
    }

    // Modify the tracked README.md to simulate an experiment edit.
    fs::write(dir.path().join("README.md"), b"modified content").unwrap();

    // Log with kept=false — should git reset --hard to head.
    let result = tokio_test::block_on(LogExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "run_id": "r1",
            "metric_value": 99.0,
            "kept": false,
            "commit_before": head,
            "idea": "test-revert",
            "duration_ms": 100
        }),
    ))
    .unwrap();

    assert!(!result.is_error, "log tool should not error; got: {}", result.model_output);
    // After git reset --hard, the tracked README.md should be back to "init".
    let contents = fs::read_to_string(dir.path().join("README.md")).unwrap_or_default();
    assert_eq!(contents, "init", "reset --hard should have reverted README.md to 'init'");
}

// ── LogExperimentTool: kept=true commits ──────────────────────────────────────

#[cfg(unix)]
#[test]
fn log_tool_kept_true_commits_changes() {
    let dir = TempDir::new().unwrap();
    git_init_with_commit(dir.path());

    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "name": "keep-test",
            "metric": "fps",
            "unit": "f/s",
            "direction": "higher",
            "working_dir": dir.path().to_str().unwrap()
        }),
    ))
    .unwrap();

    let head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // Skip if git is unavailable or commit signing failed.
    if head.is_empty() {
        return;
    }

    // Write a new file to commit.
    fs::write(dir.path().join("improvement.txt"), b"better").unwrap();

    let result = tokio_test::block_on(LogExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "run_id": "r2",
            "metric_value": 120.0,
            "kept": true,
            "commit_before": head,
            "idea": "speed improvement",
            "duration_ms": 200
        }),
    ))
    .unwrap();

    assert!(!result.is_error, "kept=true should succeed; got: {}", result.model_output);

    // A new commit should exist.
    let new_head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // Only assert if we got a valid new HEAD (commit may fail in some envs).
    if !new_head.is_empty() {
        assert_ne!(head, new_head, "kept=true should have created a new commit");
    }
}

// ── LogExperimentTool: Result entry appended to JSONL ─────────────────────────

#[test]
fn log_tool_appends_result_entry() {
    let dir = TempDir::new().unwrap();
    tokio_test::block_on(InitExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "name": "entry-test",
            "metric": "m",
            "unit": "u",
            "direction": "lower"
        }),
    ))
    .unwrap();

    let result = tokio_test::block_on(LogExperimentTool.invoke(
        &ctx(&dir),
        "id",
        json!({
            "root": dir.path().to_str().unwrap(),
            "run_id": "my-run-id",
            "metric_value": 55.5,
            "kept": false,
            "commit_before": "abc123",
            "idea": "entry-test-idea",
            "duration_ms": 50
        }),
    ))
    .unwrap();

    assert!(!result.is_error, "log tool should succeed; got: {}", result.model_output);

    let log = JsonlLog::new(dir.path().join("autoresearch.jsonl"), MetricDirection::Lower);
    let entries = log.read_all().unwrap();
    let result_entries: Vec<_> = entries
        .iter()
        .filter(|e| matches!(&e.kind, LogEntryKind::Result { .. }))
        .collect();
    assert_eq!(result_entries.len(), 1);
    if let LogEntryKind::Result { metric_value, kept, .. } = result_entries[0].kind {
        assert!((metric_value - 55.5).abs() < 1e-9);
        assert!(!kept);
    }
}
