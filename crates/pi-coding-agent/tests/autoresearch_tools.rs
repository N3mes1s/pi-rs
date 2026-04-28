//! Integration tests for the upstream-faithful `init_experiment`,
//! `run_experiment`, `log_experiment` tools.

use pi_coding_agent::autoresearch::{
    InitExperimentTool, JsonlLog, LogExperimentTool, RunExperimentTool, RunStatus,
};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::path::PathBuf;

fn ctx(p: &PathBuf) -> ToolContext {
    ToolContext {
        cwd: p.clone(),
        max_output_bytes: 64 * 1024,
    }
}

#[tokio::test]
async fn init_experiment_writes_config_header() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    let tool = InitExperimentTool;

    let r = tool
        .invoke(
            &ctx(&cwd),
            "1",
            json!({
                "name": "test-session",
                "metric_name": "build_ms",
                "metric_unit": "ms",
                "direction": "lower"
            }),
        )
        .await
        .unwrap();
    assert!(!r.is_error);
    let log = JsonlLog::new(cwd.join("autoresearch.jsonl"));
    let cfg = log.read_latest_config().unwrap().unwrap();
    assert_eq!(cfg.name.as_deref(), Some("test-session"));
    assert_eq!(cfg.metric_name.as_deref(), Some("build_ms"));
}

#[tokio::test]
async fn run_experiment_parses_metric_lines_and_passes_checks() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    // Simulate a benchmark that emits METRIC lines.
    let bench = cwd.join("autoresearch.sh");
    std::fs::write(
        &bench,
        "#!/bin/bash\necho hello\necho 'METRIC startup_us=1500'\necho 'METRIC size_kib=4000'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&bench).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&bench, p).unwrap();
    }

    // No checks.sh -> checks_pass should be None.
    let r = RunExperimentTool
        .invoke(
            &ctx(&cwd),
            "1",
            json!({"command": format!("bash {}", bench.display())}),
        )
        .await
        .unwrap();
    assert!(!r.is_error);
    let display = r.display.unwrap();
    assert_eq!(display["primary_metric"], 1500.0);
    assert_eq!(display["metrics"]["size_kib"], 4000.0);
    assert_eq!(display["passed"], true);
    assert!(display["checks_pass"].is_null());
}

#[tokio::test]
async fn run_experiment_runs_checks_after_passing_benchmark() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    let bench = cwd.join("autoresearch.sh");
    std::fs::write(&bench, "#!/bin/bash\necho 'METRIC m=1'\n").unwrap();
    let checks = cwd.join("autoresearch.checks.sh");
    std::fs::write(&checks, "#!/bin/bash\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&bench, &checks] {
            let mut perms = std::fs::metadata(p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(p, perms).unwrap();
        }
    }

    let r = RunExperimentTool
        .invoke(
            &ctx(&cwd),
            "1",
            json!({"command": format!("bash {}", bench.display())}),
        )
        .await
        .unwrap();
    let display = r.display.unwrap();
    assert_eq!(display["primary_metric"], 1.0);
    assert_eq!(display["checks_pass"], true);
}

#[tokio::test]
async fn run_experiment_marks_checks_failed_when_checks_exit_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    let bench = cwd.join("autoresearch.sh");
    std::fs::write(&bench, "#!/bin/bash\necho 'METRIC m=1'\n").unwrap();
    let checks = cwd.join("autoresearch.checks.sh");
    std::fs::write(&checks, "#!/bin/bash\necho 'oops' >&2; exit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&bench, &checks] {
            let mut perms = std::fs::metadata(p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(p, perms).unwrap();
        }
    }

    let r = RunExperimentTool
        .invoke(
            &ctx(&cwd),
            "1",
            json!({"command": format!("bash {}", bench.display())}),
        )
        .await
        .unwrap();
    let display = r.display.unwrap();
    assert_eq!(display["checks_pass"], false);
    assert!(r.model_output.contains("checks_output"));
}

#[tokio::test]
async fn log_experiment_appends_run_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    // init first
    InitExperimentTool
        .invoke(
            &ctx(&cwd),
            "1",
            json!({"name": "s", "metric_name": "m", "direction": "lower"}),
        )
        .await
        .unwrap();

    // Make the cwd a git repo so the discard reset doesn't error.
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&cwd)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@e",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(&cwd)
        .status()
        .unwrap();
    let head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&cwd)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let r = LogExperimentTool
        .invoke(
            &ctx(&cwd),
            "1",
            json!({
                "commit": head,
                "metric": 42.0,
                "status": "discard",
                "description": "tried something silly",
                "metrics": {"size_kib": 5000},
                "asi": {"learned": "noise floor matters"}
            }),
        )
        .await
        .unwrap();
    assert!(!r.is_error);
    let log = JsonlLog::new(cwd.join("autoresearch.jsonl"));
    let runs = log.read_runs().unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.run, 1);
    assert_eq!(run.metric, 42.0);
    assert_eq!(run.status, RunStatus::Discard);
    assert_eq!(run.description, "tried something silly");
    assert_eq!(run.metrics.get("size_kib").copied(), Some(5000.0));
    assert!(run.asi.is_some());
}
