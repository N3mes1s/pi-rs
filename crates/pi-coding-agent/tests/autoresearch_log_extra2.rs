//! Extra coverage for autoresearch::log (second round).
//!
//! Covers:
//! - read_all with malformed JSON line → Err(InvalidData)
//! - read_all skips blank lines
//! - best_result with single kept value returns that value
//! - count_kept_results: many kept=true and many kept=false entries
//! - append multiple kinds in rapid succession (unique id check)

use std::io;
use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    log::{JsonlLog, LogEntryKind},
    session::{MetricDirection, SessionConfig},
};

fn lower_log(dir: &TempDir) -> JsonlLog {
    JsonlLog::new(dir.path().join("test.jsonl"), MetricDirection::Lower)
}

fn sample_config() -> SessionConfig {
    SessionConfig {
        name: "extra2".to_string(),
        metric: "ms".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    }
}

// ── read_all: malformed JSON line → Err(InvalidData) ─────────────────────────

#[test]
fn read_all_malformed_json_line_returns_err() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonl");
    // Write a malformed JSON line.
    std::fs::write(&path, b"not valid json\n").unwrap();

    let log = JsonlLog::new(&path, MetricDirection::Lower);
    let result = log.read_all();
    assert!(result.is_err(), "malformed JSON should return Err");
    if let Err(e) = result {
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}

// ── read_all: blank lines are skipped ────────────────────────────────────────

#[test]
fn read_all_skips_blank_lines() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);

    // Append one entry normally.
    log.append(LogEntryKind::Stop { reason: "done".into() }).unwrap();

    // Add a blank line manually.
    let path = dir.path().join("test.jsonl");
    let existing = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{}\n\n   \n", existing)).unwrap();

    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 1, "blank lines should be skipped");
}

// ── best_result: single kept entry ────────────────────────────────────────────

#[test]
fn best_result_single_kept_returns_that_value() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let run = log
        .append(LogEntryKind::Run {
            idea: "x".into(),
            commit_before: "a".into(),
        })
        .unwrap();
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 42.0,
        duration_ms: 0,
        kept: true,
        commit_after: "b".into(),
        checks_passed: true,
    })
    .unwrap();
    assert_eq!(log.best_result().unwrap(), Some(42.0));
}

// ── count_kept_results: mixed kept/not-kept ────────────────────────────────────

#[test]
fn count_kept_results_mixed_entries() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let run = log
        .append(LogEntryKind::Run {
            idea: "mix".into(),
            commit_before: "aaa".into(),
        })
        .unwrap();

    let kept_values = [true, false, true, true, false, true];
    for &k in &kept_values {
        log.append(LogEntryKind::Result {
            run_id: run.id.clone(),
            metric_value: 10.0,
            duration_ms: 0,
            kept: k,
            commit_after: "bbb".into(),
            checks_passed: true,
        })
        .unwrap();
    }

    let count = log.count_kept_results().unwrap();
    let expected = kept_values.iter().filter(|&&k| k).count();
    assert_eq!(count, expected);
}

// ── Append many entries: all have unique ids ──────────────────────────────────

#[test]
fn many_appends_produce_all_unique_ids() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);

    let mut ids = std::collections::HashSet::new();
    for i in 0..10 {
        let e = log.append(LogEntryKind::Stop { reason: format!("{}", i) }).unwrap();
        ids.insert(e.id);
    }
    assert_eq!(ids.len(), 10, "all ids must be unique");
}

// ── best_result: Higher picks max ─────────────────────────────────────────────

#[test]
fn best_result_higher_fold_picks_max() {
    let dir = TempDir::new().unwrap();
    let log = JsonlLog::new(dir.path().join("h.jsonl"), MetricDirection::Higher);
    let run = log
        .append(LogEntryKind::Run {
            idea: "h".into(),
            commit_before: "aaa".into(),
        })
        .unwrap();

    for v in [10.0_f64, 30.0, 20.0] {
        log.append(LogEntryKind::Result {
            run_id: run.id.clone(),
            metric_value: v,
            duration_ms: 0,
            kept: true,
            commit_after: "bbb".into(),
            checks_passed: true,
        })
        .unwrap();
    }
    assert_eq!(log.best_result().unwrap(), Some(30.0));
}

// ── JsonlLog::new with Into<PathBuf> from &str ────────────────────────────────

#[test]
fn jsonl_log_new_from_str_path() {
    let dir = TempDir::new().unwrap();
    let path_str = dir.path().join("str_test.jsonl").to_str().unwrap().to_string();
    let log = JsonlLog::new(path_str, MetricDirection::Lower);
    let e = log.append(LogEntryKind::Stop { reason: "str".into() }).unwrap();
    assert!(!e.id.is_empty());
}
