//! Tests for the `/autoresearch` slash-command helper layer.
//!
//! These tests drive the pure helpers in
//! `autoresearch::slash_helpers` without shelling out to git or
//! spawning a real agent session.

use std::fs;

use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    session::{MetricDirection, Session, SessionConfig},
    slash_helpers::{
        clear_artefacts, ensure_session, export_dashboard, parse_action, AutoresearchAction,
    },
};

// ── parse_action ──────────────────────────────────────────────────────────────

#[test]
fn parse_action_off() {
    assert_eq!(parse_action("off"), AutoresearchAction::Off);
}

#[test]
fn parse_action_clear() {
    assert_eq!(parse_action("clear"), AutoresearchAction::Clear);
}

#[test]
fn parse_action_export() {
    assert_eq!(parse_action("export"), AutoresearchAction::Export);
}

#[test]
fn parse_action_start_with_text() {
    assert_eq!(
        parse_action("optimise the loop"),
        AutoresearchAction::Start {
            text: "optimise the loop".to_string()
        }
    );
}

#[test]
fn parse_action_start_empty_args() {
    assert_eq!(
        parse_action(""),
        AutoresearchAction::Start {
            text: "".to_string()
        }
    );
}

#[test]
fn parse_action_trims_whitespace() {
    assert_eq!(parse_action("  off  "), AutoresearchAction::Off);
    assert_eq!(parse_action("  clear  "), AutoresearchAction::Clear);
}

// ── ensure_session: new session ───────────────────────────────────────────────

#[test]
fn ensure_session_creates_config_when_absent() {
    let dir = TempDir::new().unwrap();
    let session = ensure_session(dir.path(), "my-experiment").unwrap();
    assert_eq!(session.config.name, "my-experiment");
    assert!(dir.path().join("autoresearch.config.json").exists());
    assert!(dir.path().join("autoresearch.md").exists());
}

#[test]
fn ensure_session_new_uses_lower_direction_by_default() {
    let dir = TempDir::new().unwrap();
    let session = ensure_session(dir.path(), "speed-test").unwrap();
    assert_eq!(session.config.direction, MetricDirection::Lower);
}

// ── ensure_session: resume existing ──────────────────────────────────────────

#[test]
fn ensure_session_loads_existing_config() {
    let dir = TempDir::new().unwrap();

    // Pre-write a config.
    let config = SessionConfig {
        name: "existing-exp".to_string(),
        metric: "latency".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Higher,
        max_iterations: Some(5),
        working_dir: None,
    };
    let session = Session::new(dir.path(), config);
    session.save_config().unwrap();

    // ensure_session should load the existing config (resume path).
    let loaded = ensure_session(dir.path(), "ignored-text").unwrap();
    assert_eq!(loaded.config.name, "existing-exp", "name from existing config");
    assert_eq!(loaded.config.direction, MetricDirection::Higher);
    assert_eq!(loaded.config.max_iterations, Some(5));
}

// ── clear_artefacts ───────────────────────────────────────────────────────────

#[test]
fn clear_artefacts_removes_all_three_files() {
    let dir = TempDir::new().unwrap();

    // Create the three artefact files.
    for name in &[
        "autoresearch.jsonl",
        "autoresearch.md",
        "autoresearch.config.json",
    ] {
        fs::write(dir.path().join(name), b"dummy").unwrap();
    }

    let removed = clear_artefacts(dir.path());
    assert_eq!(removed.len(), 3, "should remove 3 files; got: {removed:?}");

    // All three must be gone.
    for name in &[
        "autoresearch.jsonl",
        "autoresearch.md",
        "autoresearch.config.json",
    ] {
        assert!(
            !dir.path().join(name).exists(),
            "{name} should have been removed"
        );
    }
}

#[test]
fn clear_artefacts_tolerates_missing_files() {
    let dir = TempDir::new().unwrap();
    // Only create one of the three files.
    fs::write(dir.path().join("autoresearch.jsonl"), b"x").unwrap();

    let removed = clear_artefacts(dir.path());
    assert_eq!(removed.len(), 1, "only one file existed; got: {removed:?}");
}

#[test]
fn clear_artefacts_returns_empty_when_nothing_to_remove() {
    let dir = TempDir::new().unwrap();
    let removed = clear_artefacts(dir.path());
    assert!(removed.is_empty());
}

// ── full /autoresearch <text> flow ────────────────────────────────────────────

#[test]
fn start_flow_creates_session_then_clear_removes_all() {
    let dir = TempDir::new().unwrap();

    // 1. "Enter" — ensure_session creates config + md.
    let session = ensure_session(dir.path(), "start-then-clear").unwrap();
    assert_eq!(session.config.name, "start-then-clear");
    assert!(dir.path().join("autoresearch.config.json").exists());
    assert!(dir.path().join("autoresearch.md").exists());

    // 2. Write a fake jsonl file to simulate logged runs.
    fs::write(dir.path().join("autoresearch.jsonl"), b"{}").unwrap();

    // 3. "Clear" — must remove all three artefacts.
    let removed = clear_artefacts(dir.path());
    assert_eq!(removed.len(), 3, "expected 3 removed; got: {removed:?}");
    assert!(!dir.path().join("autoresearch.config.json").exists());
    assert!(!dir.path().join("autoresearch.jsonl").exists());
    assert!(!dir.path().join("autoresearch.md").exists());
}

// ── export_dashboard ──────────────────────────────────────────────────────────

#[test]
fn export_dashboard_no_session_returns_error() {
    let dir = TempDir::new().unwrap();
    // No config.json → should return Err.
    let result = export_dashboard(dir.path());
    assert!(result.is_err(), "export without session should fail");
}

#[test]
fn export_dashboard_creates_html_file() {
    let dir = TempDir::new().unwrap();

    // Bootstrap a minimal session (no runs).
    ensure_session(dir.path(), "export-test").unwrap();

    let path = export_dashboard(dir.path()).expect("export should succeed");
    assert_eq!(path, dir.path().join("autoresearch-dashboard.html"));
    assert!(path.exists(), "HTML file should exist");

    let html = fs::read_to_string(&path).unwrap();
    assert!(html.contains("export-test"), "HTML should contain session name; got: {html}");
    assert!(html.contains("<pre>"), "HTML should contain <pre> tag; got: {html}");
}

#[test]
fn export_dashboard_html_contains_session_name() {
    let dir = TempDir::new().unwrap();
    ensure_session(dir.path(), "named-session").unwrap();

    let path = export_dashboard(dir.path()).unwrap();
    let html = fs::read_to_string(&path).unwrap();
    assert!(
        html.contains("named-session"),
        "HTML must reference the session name; got: {html}"
    );
}
