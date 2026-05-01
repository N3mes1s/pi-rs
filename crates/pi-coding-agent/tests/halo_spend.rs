//! Unit tests for `halo::spend::today_spend` — timestamp filtering and
//! `supersedes`-based deduplication.

use chrono::{Duration, Utc};
use std::io::Write;
use tempfile::NamedTempFile;

fn today_ts() -> String {
    Utc::now().to_rfc3339()
}

fn yesterday_ts() -> String {
    (Utc::now() - Duration::hours(25)).to_rfc3339()
}

fn write_usage(rows: &[serde_json::Value]) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    for row in rows {
        let mut line = serde_json::to_string(row).unwrap();
        line.push('\n');
        f.write_all(line.as_bytes()).unwrap();
    }
    f
}

// ---------------------------------------------------------------------------
// Basic summing
// ---------------------------------------------------------------------------

#[test]
fn empty_file_returns_zero() {
    let f = write_usage(&[]);
    assert_eq!(pi_coding_agent::halo::spend::today_spend(f.path()), 0.0);
}

#[test]
fn single_today_row_is_counted() {
    let f = write_usage(&[serde_json::json!({
        "kind": "evolve_tick",
        "ts": today_ts(),
        "cost_usd": 1.23,
    })]);
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 1.23).abs() < 1e-9, "expected 1.23, got {spend}");
}

#[test]
fn multiple_today_rows_are_summed() {
    let f = write_usage(&[
        serde_json::json!({"ts": today_ts(), "cost_usd": 1.00}),
        serde_json::json!({"ts": today_ts(), "cost_usd": 2.50}),
    ]);
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 3.50).abs() < 1e-9, "expected 3.50, got {spend}");
}

// ---------------------------------------------------------------------------
// Timestamp filtering: yesterday's rows must be excluded
// ---------------------------------------------------------------------------

#[test]
fn yesterday_row_is_excluded() {
    let f = write_usage(&[serde_json::json!({
        "ts": yesterday_ts(),
        "cost_usd": 99.0,
    })]);
    assert_eq!(pi_coding_agent::halo::spend::today_spend(f.path()), 0.0);
}

#[test]
fn mixed_today_and_yesterday_sums_only_today() {
    let f = write_usage(&[
        serde_json::json!({"ts": today_ts(),     "cost_usd": 5.00}),
        serde_json::json!({"ts": yesterday_ts(), "cost_usd": 100.0}),
        serde_json::json!({"ts": today_ts(),     "cost_usd": 3.00}),
    ]);
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 8.00).abs() < 1e-9, "expected 8.00, got {spend}");
}

// ---------------------------------------------------------------------------
// Deduplication: rows with `supersedes` (non-null) are skipped
// ---------------------------------------------------------------------------

#[test]
fn supersedes_row_is_excluded() {
    let f = write_usage(&[serde_json::json!({
        "ts": today_ts(),
        "cost_usd": 5.00,
        "supersedes": "some-prior-row-id",
    })]);
    // The supersedes row itself is a correction and should not be counted.
    assert_eq!(pi_coding_agent::halo::spend::today_spend(f.path()), 0.0);
}

#[test]
fn supersedes_null_is_not_excluded() {
    let f = write_usage(&[serde_json::json!({
        "ts": today_ts(),
        "cost_usd": 7.77,
        "supersedes": null,
    })]);
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 7.77).abs() < 1e-9, "expected 7.77, got {spend}");
}

#[test]
fn dedup_skips_correction_but_keeps_plain_rows() {
    // One plain row (1.00) and one correction row (50.00, supersedes something).
    // Only the plain row should be counted.
    let f = write_usage(&[
        serde_json::json!({"ts": today_ts(), "cost_usd": 1.00}),
        serde_json::json!({"ts": today_ts(), "cost_usd": 50.00, "supersedes": "row-abc"}),
    ]);
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 1.00).abs() < 1e-9, "expected 1.00, got {spend}");
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn missing_file_returns_zero() {
    let path = std::path::Path::new("/tmp/pi-halo-spend-nonexistent-file-xyz.jsonl");
    assert_eq!(pi_coding_agent::halo::spend::today_spend(path), 0.0);
}

#[test]
fn malformed_lines_are_skipped() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"not-json\n").unwrap();
    f.write_all(
        serde_json::to_string(&serde_json::json!({"ts": today_ts(), "cost_usd": 2.00}))
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    f.write_all(b"\n").unwrap();
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 2.00).abs() < 1e-9, "expected 2.00, got {spend}");
}

#[test]
fn row_missing_cost_usd_contributes_zero() {
    let f = write_usage(&[
        serde_json::json!({"ts": today_ts(), "kind": "no_cost_field"}),
        serde_json::json!({"ts": today_ts(), "cost_usd": 4.00}),
    ]);
    let spend = pi_coding_agent::halo::spend::today_spend(f.path());
    assert!((spend - 4.00).abs() < 1e-9, "expected 4.00, got {spend}");
}
