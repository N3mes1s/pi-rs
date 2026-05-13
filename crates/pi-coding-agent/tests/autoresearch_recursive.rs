//! Tests for `run_experiment_recursive` (RAO, RFD 0032).
//!
//! Verifies:
//! 1. The tool runs the parent + all child commands in parallel.
//! 2. The delegation bonus is computed correctly from child success rates.
//! 3. The composite metric is adjusted in the right direction.
//! 4. Error paths (missing fields, failed commands) are handled gracefully.
//! 5. The RunEntry log schema accepts the new `depth`, `delegation_bonus`,
//!    `child_run_ids` fields.

use pi_coding_agent::autoresearch::{JsonlLog, RunExperimentRecursiveTool};
use pi_coding_agent::autoresearch::log::{BestDirection, ConfigEntry, RunEntry, RunStatus};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::path::PathBuf;

fn ctx(p: &PathBuf) -> ToolContext {
    ToolContext {
        cwd: p.clone(),
        max_output_bytes: 64 * 1024,
    }
}

/// Write a small benchmark script that echoes a METRIC line.
fn write_bench(dir: &std::path::Path, name: &str, metric_val: &str) -> PathBuf {
    let script = dir.join(name);
    std::fs::write(
        &script,
        format!("#!/bin/bash\necho 'METRIC score={metric_val}'\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

// ── 1. Basic delegation bonus computation ────────────────────────────────────

#[tokio::test]
async fn recursive_tool_computes_delegation_bonus_when_children_improve() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    // Parent: 900 (below baseline 1000 → improved if lower-is-better)
    write_bench(&cwd, "parent.sh", "900");
    // Child A: 800 (improved over baseline 1000)
    write_bench(&cwd, "child_a.sh", "800");
    // Child B: 1100 (worse than baseline 1000)
    write_bench(&cwd, "child_b.sh", "1100");

    let tool = RunExperimentRecursiveTool;
    let r = tool
        .invoke(
            &ctx(&cwd),
            "call-1",
            json!({
                "parent_command":  "bash parent.sh",
                "parent_baseline": 1000.0,
                "sub_experiments": [
                    {"id": "child-a", "command": "bash child_a.sh", "baseline": 1000.0},
                    {"id": "child-b", "command": "bash child_b.sh", "baseline": 1000.0},
                ],
                "lambda": 0.4,
                "direction": "lower",
            }),
        )
        .await
        .unwrap();

    assert!(!r.is_error, "parent passed → should not be error");

    let display = r.display.as_ref().unwrap();
    // parent_metric = 900
    assert_eq!(display["parent_metric"].as_f64().unwrap(), 900.0);
    // Only child-a improved → mean_success = 0.5
    let mean = display["mean_child_success"].as_f64().unwrap();
    assert!((mean - 0.5).abs() < 1e-9, "mean_child_success should be 0.5, got {mean}");
    // delegation_bonus = 0.4 × 0.5 = 0.2
    let bonus = display["delegation_bonus"].as_f64().unwrap();
    assert!((bonus - 0.2).abs() < 1e-9, "delegation_bonus should be 0.2, got {bonus}");
    // composite < parent_metric (lower direction, bonus further reduces it)
    let composite = display["composite_metric"].as_f64().unwrap();
    assert!(
        composite < 900.0,
        "composite {composite} should be < parent_metric 900"
    );
}

// ── 2. No children improve → zero bonus ────────────────────────────────────

#[tokio::test]
async fn recursive_tool_zero_bonus_when_no_children_improve() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    write_bench(&cwd, "parent.sh", "900");
    write_bench(&cwd, "child_worse.sh", "1200"); // worse than baseline 1000

    let tool = RunExperimentRecursiveTool;
    let r = tool
        .invoke(
            &ctx(&cwd),
            "call-2",
            json!({
                "parent_command":  "bash parent.sh",
                "parent_baseline": 1000.0,
                "sub_experiments": [
                    {"id": "worse", "command": "bash child_worse.sh", "baseline": 1000.0},
                ],
                "lambda": 0.4,
                "direction": "lower",
            }),
        )
        .await
        .unwrap();

    let display = r.display.as_ref().unwrap();
    let bonus = display["delegation_bonus"].as_f64().unwrap();
    assert!((bonus).abs() < 1e-9, "bonus should be 0.0 when no children improve, got {bonus}");
    // composite == parent_metric when bonus == 0
    let composite = display["composite_metric"].as_f64().unwrap();
    let parent = display["parent_metric"].as_f64().unwrap();
    assert!(
        (composite - parent).abs() < 1e-6,
        "composite {composite} should equal parent_metric {parent} when bonus=0"
    );
}

// ── 3. All children improve → full bonus ───────────────────────────────────

#[tokio::test]
async fn recursive_tool_full_bonus_when_all_children_improve() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    write_bench(&cwd, "parent.sh", "900");
    write_bench(&cwd, "child_a.sh", "700");
    write_bench(&cwd, "child_b.sh", "750");
    write_bench(&cwd, "child_c.sh", "600");

    let tool = RunExperimentRecursiveTool;
    let r = tool
        .invoke(
            &ctx(&cwd),
            "call-3",
            json!({
                "parent_command":  "bash parent.sh",
                "parent_baseline": 1000.0,
                "sub_experiments": [
                    {"id": "a", "command": "bash child_a.sh", "baseline": 1000.0},
                    {"id": "b", "command": "bash child_b.sh", "baseline": 1000.0},
                    {"id": "c", "command": "bash child_c.sh", "baseline": 1000.0},
                ],
                "lambda": 0.4,
                "direction": "lower",
            }),
        )
        .await
        .unwrap();

    let display = r.display.as_ref().unwrap();
    let mean = display["mean_child_success"].as_f64().unwrap();
    assert!((mean - 1.0).abs() < 1e-9, "all children improved → mean_success=1.0, got {mean}");
    let bonus = display["delegation_bonus"].as_f64().unwrap();
    assert!((bonus - 0.4).abs() < 1e-9, "full bonus = λ×1.0 = 0.4, got {bonus}");
}

// ── 4. direction=higher ─────────────────────────────────────────────────────

#[tokio::test]
async fn recursive_tool_direction_higher() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    // baseline=50, parent gets 80 (improved), child gets 90 (improved)
    write_bench(&cwd, "parent.sh", "80");
    write_bench(&cwd, "child.sh", "90");

    let tool = RunExperimentRecursiveTool;
    let r = tool
        .invoke(
            &ctx(&cwd),
            "call-4",
            json!({
                "parent_command":  "bash parent.sh",
                "parent_baseline": 50.0,
                "sub_experiments": [
                    {"id": "up", "command": "bash child.sh", "baseline": 50.0},
                ],
                "lambda": 0.4,
                "direction": "higher",
            }),
        )
        .await
        .unwrap();

    let display = r.display.as_ref().unwrap();
    assert_eq!(display["direction"].as_str().unwrap(), "higher");
    let composite = display["composite_metric"].as_f64().unwrap();
    let parent = display["parent_metric"].as_f64().unwrap();
    // direction=higher → composite > parent when child improved
    assert!(
        composite > parent,
        "direction=higher: composite {composite} should be > parent {parent}"
    );
}

// ── 5. Missing required field returns error ─────────────────────────────────

#[tokio::test]
async fn recursive_tool_errors_on_missing_parent_command() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    let tool = RunExperimentRecursiveTool;
    let result = tool
        .invoke(
            &ctx(&cwd),
            "call-5",
            json!({
                // parent_command missing intentionally
                "parent_baseline": 1000.0,
                "sub_experiments": [{"id": "x", "command": "echo METRIC score=1", "baseline": 1000.0}],
            }),
        )
        .await;
    // Should return Err(ToolError::InvalidInput)
    assert!(result.is_err(), "missing parent_command should fail");
}

// ── 6. Failed parent → is_error true ───────────────────────────────────────

#[tokio::test]
async fn recursive_tool_sets_is_error_when_parent_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();

    // Parent script exits 1 (failure)
    let script = cwd.join("fail.sh");
    std::fs::write(&script, "#!/bin/bash\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let tool = RunExperimentRecursiveTool;
    let r = tool
        .invoke(
            &ctx(&cwd),
            "call-6",
            json!({
                "parent_command":  "bash fail.sh",
                "parent_baseline": 1000.0,
                "sub_experiments": [
                    {"id": "ok", "command": "echo 'METRIC score=900'", "baseline": 1000.0},
                ],
            }),
        )
        .await
        .unwrap();

    assert!(r.is_error, "failed parent → is_error should be true");
}

// ── 7. RunEntry schema: depth, delegation_bonus, child_run_ids ──────────────

#[test]
fn run_entry_serializes_rao_fields() {
    let entry = RunEntry {
        run: 42,
        commit: "abc1234".into(),
        metric: 900.0,
        metrics: Default::default(),
        status: RunStatus::Keep,
        description: "RAO test run".into(),
        timestamp: 0,
        confidence: None,
        iteration_tokens: None,
        asi: None,
        depth: Some(0),
        delegation_bonus: Some(0.2),
        child_run_ids: vec![43, 44],
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"depth\":0"), "depth should serialize");
    assert!(json.contains("\"delegationBonus\":0.2"), "delegation_bonus should serialize as delegationBonus");
    assert!(json.contains("\"childRunIds\":[43,44]"), "child_run_ids should serialize as childRunIds");
}

#[test]
fn run_entry_omits_rao_fields_when_absent() {
    let entry = RunEntry {
        run: 1,
        commit: "abc".into(),
        metric: 500.0,
        metrics: Default::default(),
        status: RunStatus::Discard,
        description: "baseline".into(),
        timestamp: 0,
        confidence: None,
        iteration_tokens: None,
        asi: None,
        depth: None,
        delegation_bonus: None,
        child_run_ids: vec![],
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(!json.contains("depth"), "depth should be omitted when None");
    assert!(!json.contains("delegationBonus"), "delegationBonus should be omitted when None");
    assert!(!json.contains("childRunIds"), "childRunIds should be omitted when empty");
}

// ── 8. Round-trip: write + read back ────────────────────────────────────────

#[test]
fn run_entry_round_trips_with_rao_fields() {
    let dir = tempfile::tempdir().unwrap();
    let log = JsonlLog::new(dir.path().join("autoresearch.jsonl"));

    // Write a config header first.
    log.append_config(&ConfigEntry::new("rao-test", "score", "pts", BestDirection::Lower))
        .unwrap();

    // Write a run entry with RAO fields.
    let entry = RunEntry {
        run: 7,
        commit: "deadbeef".into(),
        metric: 800.0,
        metrics: Default::default(),
        status: RunStatus::Keep,
        description: "recursive run".into(),
        timestamp: 12345,
        confidence: Some(2.5),
        iteration_tokens: None,
        asi: None,
        depth: Some(0),
        delegation_bonus: Some(0.32),
        child_run_ids: vec![8, 9, 10],
    };
    log.append_run(&entry).unwrap();

    // Read it back.
    let runs = log.read_runs().unwrap();
    assert_eq!(runs.len(), 1);
    let r = &runs[0];
    assert_eq!(r.run, 7);
    assert_eq!(r.depth, Some(0));
    assert!((r.delegation_bonus.unwrap() - 0.32).abs() < 1e-9);
    assert_eq!(r.child_run_ids, vec![8, 9, 10]);
}
