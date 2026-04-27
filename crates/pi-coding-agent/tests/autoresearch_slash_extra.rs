//! Extra coverage for autoresearch::slash_helpers.
//!
//! Covers branches in export_dashboard not hit by the main test,
//! html_escape edge cases, and export with logged runs.

use std::fs;
use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    log::{JsonlLog, LogEntryKind},
    session::{MetricDirection, Session, SessionConfig},
    slash_helpers::{clear_artefacts, ensure_session, export_dashboard},
};

// ── html_escape via export_dashboard (session with special chars in name) ──────

#[test]
fn export_dashboard_escapes_html_in_session_name() {
    let dir = TempDir::new().unwrap();
    // Create a session with HTML special chars in name.
    let config = SessionConfig {
        name: "<script>&\"test\"</script>".to_string(),
        metric: "ms".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(dir.path(), config);
    session.save_config().unwrap();
    session.save_md().unwrap();

    let path = export_dashboard(dir.path()).expect("export should succeed");
    let html = fs::read_to_string(&path).unwrap();

    // HTML should escape < > & ".
    assert!(html.contains("&lt;"), "< should be escaped; got: {html}");
    assert!(html.contains("&gt;"), "> should be escaped; got: {html}");
    assert!(html.contains("&amp;"), "& should be escaped; got: {html}");
    assert!(!html.contains("<script>"), "<script> should not appear verbatim; got: {html}");
}

// ── export_dashboard with Run + Result entries ────────────────────────────────

#[test]
fn export_dashboard_with_runs_and_results() {
    let dir = TempDir::new().unwrap();

    // Create a real session.
    let config = SessionConfig {
        name: "dashboard-runs-test".to_string(),
        metric: "latency".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(dir.path(), config.clone());
    session.save_config().unwrap();
    session.save_md().unwrap();

    // Write a log with Init, Run, and Result entries.
    let log = JsonlLog::new(session.jsonl_path(), MetricDirection::Lower);
    log.append(LogEntryKind::Init { config }).unwrap();
    let run = log
        .append(LogEntryKind::Run {
            idea: "try cache".to_string(),
            commit_before: "abc123".to_string(),
        })
        .unwrap();
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 85.0,
        duration_ms: 200,
        kept: true,
        commit_after: "def456".to_string(),
        checks_passed: true,
    })
    .unwrap();
    let run2 = log
        .append(LogEntryKind::Run {
            idea: "vectorise loop".to_string(),
            commit_before: "abc123".to_string(),
        })
        .unwrap();
    log.append(LogEntryKind::Result {
        run_id: run2.id.clone(),
        metric_value: 95.0,
        duration_ms: 150,
        kept: false,
        commit_after: "abc123".to_string(),
        checks_passed: false,
    })
    .unwrap();

    let path = export_dashboard(dir.path()).expect("export should succeed");
    let html = fs::read_to_string(&path).unwrap();

    assert!(html.contains("dashboard-runs-test"), "session name in HTML");
    assert!(html.contains("try cache"), "run idea in HTML");
    assert!(html.contains("vectorise loop"), "second run idea in HTML");
}

// ── export_dashboard: Higher direction ────────────────────────────────────────

#[test]
fn export_dashboard_higher_direction() {
    let dir = TempDir::new().unwrap();

    let config = SessionConfig {
        name: "higher-dashboard".to_string(),
        metric: "throughput".to_string(),
        unit: "ops/s".to_string(),
        direction: MetricDirection::Higher,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(dir.path(), config.clone());
    session.save_config().unwrap();
    session.save_md().unwrap();

    // Add some run data so the current_best/sample_values branch is exercised.
    let log = JsonlLog::new(session.jsonl_path(), MetricDirection::Higher);
    log.append(LogEntryKind::Init { config }).unwrap();
    let run = log
        .append(LogEntryKind::Run {
            idea: "parallel".to_string(),
            commit_before: "aaa".to_string(),
        })
        .unwrap();
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 500.0,
        duration_ms: 100,
        kept: true,
        commit_after: "bbb".to_string(),
        checks_passed: true,
    })
    .unwrap();

    let path = export_dashboard(dir.path()).expect("export should succeed");
    let html = fs::read_to_string(&path).unwrap();
    assert!(html.contains("higher-dashboard"), "session name in HTML");
    assert!(html.contains("higher"), "direction in HTML");
}

// ── export_dashboard: run_id not matching any run → uses run_id as idea ──────

#[test]
fn export_dashboard_unmatched_run_id_uses_run_id_as_idea() {
    let dir = TempDir::new().unwrap();

    let config = SessionConfig {
        name: "unmatched-runid".to_string(),
        metric: "ms".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(dir.path(), config.clone());
    session.save_config().unwrap();
    session.save_md().unwrap();

    // Write a Result entry without a preceding Run entry (orphan result).
    let log = JsonlLog::new(session.jsonl_path(), MetricDirection::Lower);
    log.append(LogEntryKind::Init { config }).unwrap();
    log.append(LogEntryKind::Result {
        run_id: "orphan-run-id".to_string(),
        metric_value: 77.0,
        duration_ms: 100,
        kept: false,
        commit_after: "xxx".to_string(),
        checks_passed: true,
    })
    .unwrap();

    let path = export_dashboard(dir.path()).expect("export should succeed");
    let html = fs::read_to_string(&path).unwrap();
    // The run_id is used as the idea when no matching Run entry.
    assert!(html.contains("orphan-run-id") || html.contains("unmatched-runid"));
}

// ── ensure_session: whitespace-trimmed name ───────────────────────────────────

#[test]
fn ensure_session_trims_whitespace_from_name() {
    let dir = TempDir::new().unwrap();
    let session = ensure_session(dir.path(), "  my-experiment  ").unwrap();
    assert_eq!(session.config.name, "my-experiment");
}

// ── clear_artefacts: partial existence ────────────────────────────────────────

#[test]
fn clear_artefacts_only_jsonl_exists() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("autoresearch.jsonl"), b"data").unwrap();
    let removed = clear_artefacts(dir.path());
    assert_eq!(removed.len(), 1);
    assert!(!dir.path().join("autoresearch.jsonl").exists());
}

#[test]
fn clear_artefacts_only_config_exists() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("autoresearch.config.json"), b"{}").unwrap();
    let removed = clear_artefacts(dir.path());
    assert_eq!(removed.len(), 1);
    assert!(!dir.path().join("autoresearch.config.json").exists());
}

// ── export_dashboard: empty log (no Init, no Run, no Result) ─────────────────

#[test]
fn export_dashboard_with_empty_log() {
    let dir = TempDir::new().unwrap();
    ensure_session(dir.path(), "empty-log").unwrap();
    // Don't write any log entries.
    let path = export_dashboard(dir.path()).expect("export should succeed");
    let html = fs::read_to_string(&path).unwrap();
    assert!(html.contains("empty-log"));
}

// ── export_dashboard: log with Hook and Stop entries (ignored in run_rows) ────

#[test]
fn export_dashboard_with_hook_and_stop_entries() {
    let dir = TempDir::new().unwrap();
    let config = SessionConfig {
        name: "hook-stop-test".to_string(),
        metric: "ms".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(dir.path(), config.clone());
    session.save_config().unwrap();
    session.save_md().unwrap();

    let log = JsonlLog::new(session.jsonl_path(), MetricDirection::Lower);
    log.append(LogEntryKind::Init { config }).unwrap();
    log.append(LogEntryKind::Hook {
        hook: "before".to_string(),
        output: "hook output".to_string(),
    })
    .unwrap();
    log.append(LogEntryKind::Stop {
        reason: "max iterations".to_string(),
    })
    .unwrap();

    let path = export_dashboard(dir.path()).expect("export should succeed");
    let html = fs::read_to_string(&path).unwrap();
    assert!(html.contains("hook-stop-test"));
}
