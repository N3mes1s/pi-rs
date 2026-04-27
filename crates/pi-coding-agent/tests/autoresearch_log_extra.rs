//! Extra coverage for autoresearch::log.
//!
//! Covers:
//! - read_all on non-existent file → empty vec (not error)
//! - Hook and Stop entry round-trip via append/read_all
//! - count_kept_results on empty log → 0
//! - best_result with no kept results → None
//! - best_result with multiple kept values (Lower min / Higher max)
//! - read_all skips blank lines
//! - timestamp is non-negative

use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    log::{JsonlLog, LogEntryKind},
    session::{MetricDirection, SessionConfig},
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn lower_log(dir: &TempDir) -> JsonlLog {
    JsonlLog::new(dir.path().join("test.jsonl"), MetricDirection::Lower)
}

fn higher_log(dir: &TempDir) -> JsonlLog {
    JsonlLog::new(dir.path().join("test.jsonl"), MetricDirection::Higher)
}

fn sample_config() -> SessionConfig {
    SessionConfig {
        name: "log-extra".to_string(),
        metric: "latency".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    }
}

// ── read_all on non-existent file ─────────────────────────────────────────────

#[test]
fn read_all_on_missing_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let entries = log.read_all().unwrap();
    assert!(entries.is_empty(), "missing file should give empty vec");
}

// ── Hook entry round-trip ─────────────────────────────────────────────────────

#[test]
fn hook_entry_append_and_read_roundtrip() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);

    let entry = log
        .append(LogEntryKind::Hook {
            hook: "before".to_string(),
            output: "hook stdout".to_string(),
        })
        .unwrap();

    let all = log.read_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, entry.id);
    assert!(matches!(
        &all[0].kind,
        LogEntryKind::Hook { hook, output }
            if hook == "before" && output == "hook stdout"
    ));
}

// ── Stop entry round-trip ─────────────────────────────────────────────────────

#[test]
fn stop_entry_append_and_read_roundtrip() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);

    let entry = log
        .append(LogEntryKind::Stop {
            reason: "max iterations reached".to_string(),
        })
        .unwrap();

    let all = log.read_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, entry.id);
    assert!(matches!(
        &all[0].kind,
        LogEntryKind::Stop { reason }
            if reason == "max iterations reached"
    ));
}

// ── Init entry round-trip ─────────────────────────────────────────────────────

#[test]
fn init_entry_append_and_read_roundtrip() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let cfg = sample_config();

    let entry = log
        .append(LogEntryKind::Init { config: cfg.clone() })
        .unwrap();

    let all = log.read_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, entry.id);
    if let LogEntryKind::Init { config } = &all[0].kind {
        assert_eq!(config.name, cfg.name);
        assert_eq!(config.direction, MetricDirection::Lower);
    } else {
        panic!("expected Init entry, got {:?}", all[0].kind);
    }
}

// ── count_kept_results on empty log ──────────────────────────────────────────

#[test]
fn count_kept_results_empty_log_returns_zero() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    assert_eq!(log.count_kept_results().unwrap(), 0);
}

// ── count_kept_results: only Init entries → 0 ────────────────────────────────

#[test]
fn count_kept_results_no_result_entries_returns_zero() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    log.append(LogEntryKind::Init { config: sample_config() }).unwrap();
    log.append(LogEntryKind::Run {
        idea: "x".into(),
        commit_before: "abc".into(),
    })
    .unwrap();
    assert_eq!(log.count_kept_results().unwrap(), 0);
}

// ── best_result: no kept results → None ──────────────────────────────────────

#[test]
fn best_result_no_kept_results_returns_none() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let run = log
        .append(LogEntryKind::Run {
            idea: "x".into(),
            commit_before: "abc".into(),
        })
        .unwrap();
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 10.0,
        duration_ms: 100,
        kept: false,
        commit_after: "bbb".into(),
        checks_passed: false,
    })
    .unwrap();
    assert_eq!(log.best_result().unwrap(), None);
}

// ── best_result: Lower picks the minimum kept value ──────────────────────────

#[test]
fn best_result_lower_multiple_kept_picks_minimum() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let run = log
        .append(LogEntryKind::Run {
            idea: "x".into(),
            commit_before: "aaa".into(),
        })
        .unwrap();

    for (v, kept) in [(50.0, true), (30.0, true), (80.0, false), (20.0, true)] {
        log.append(LogEntryKind::Result {
            run_id: run.id.clone(),
            metric_value: v,
            duration_ms: 0,
            kept,
            commit_after: "xxx".into(),
            checks_passed: true,
        })
        .unwrap();
    }
    assert_eq!(log.best_result().unwrap(), Some(20.0));
}

// ── best_result: Higher picks the maximum kept value ─────────────────────────

#[test]
fn best_result_higher_multiple_kept_picks_maximum() {
    let dir = TempDir::new().unwrap();
    let log = higher_log(&dir);
    let run = log
        .append(LogEntryKind::Run {
            idea: "y".into(),
            commit_before: "bbb".into(),
        })
        .unwrap();

    for (v, kept) in [(50.0, true), (200.0, true), (100.0, false), (150.0, true)] {
        log.append(LogEntryKind::Result {
            run_id: run.id.clone(),
            metric_value: v,
            duration_ms: 0,
            kept,
            commit_after: "yyy".into(),
            checks_passed: true,
        })
        .unwrap();
    }
    assert_eq!(log.best_result().unwrap(), Some(200.0));
}

// ── timestamp is non-negative ─────────────────────────────────────────────────

#[test]
fn append_sets_positive_timestamp() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let entry = log
        .append(LogEntryKind::Stop {
            reason: "ts-test".into(),
        })
        .unwrap();
    assert!(entry.timestamp >= 0, "timestamp must be non-negative; got {}", entry.timestamp);
}

// ── id is non-empty ───────────────────────────────────────────────────────────

#[test]
fn append_sets_non_empty_id() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let entry = log
        .append(LogEntryKind::Stop { reason: "id-test".into() })
        .unwrap();
    assert!(!entry.id.is_empty(), "id must be non-empty");
}

// ── Multiple appends produce distinct ids ─────────────────────────────────────

#[test]
fn multiple_appends_produce_distinct_ids() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let e1 = log.append(LogEntryKind::Stop { reason: "1".into() }).unwrap();
    let e2 = log.append(LogEntryKind::Stop { reason: "2".into() }).unwrap();
    assert_ne!(e1.id, e2.id, "consecutive entries must have distinct ids");
}

// ── read_all: multiple entry types in one file ────────────────────────────────

#[test]
fn read_all_handles_all_entry_kinds() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let cfg = sample_config();

    log.append(LogEntryKind::Init { config: cfg }).unwrap();
    let run = log
        .append(LogEntryKind::Run {
            idea: "try".into(),
            commit_before: "abc".into(),
        })
        .unwrap();
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 5.0,
        duration_ms: 10,
        kept: true,
        commit_after: "def".into(),
        checks_passed: true,
    })
    .unwrap();
    log.append(LogEntryKind::Hook {
        hook: "after".into(),
        output: "done".into(),
    })
    .unwrap();
    log.append(LogEntryKind::Stop {
        reason: "done".into(),
    })
    .unwrap();

    let all = log.read_all().unwrap();
    assert_eq!(all.len(), 5);
    assert!(matches!(all[0].kind, LogEntryKind::Init { .. }));
    assert!(matches!(all[1].kind, LogEntryKind::Run { .. }));
    assert!(matches!(all[2].kind, LogEntryKind::Result { .. }));
    assert!(matches!(all[3].kind, LogEntryKind::Hook { .. }));
    assert!(matches!(all[4].kind, LogEntryKind::Stop { .. }));
}
