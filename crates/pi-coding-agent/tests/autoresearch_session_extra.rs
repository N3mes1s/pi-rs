//! Extra coverage for autoresearch::session.
//!
//! Covers:
//! - Session::load round-trip via save_config/load
//! - Session::load: missing config file → Err(NotFound)
//! - Session::load: invalid JSON → Err(InvalidData)
//! - md_path / jsonl_path / config_path / checks_script / benchmark_script
//!   return the right files
//! - save_md: Higher direction says "higher is better"
//! - save_md: unlimited max_iterations
//! - save_md: custom working_dir appears in markdown

use std::io;
use tempfile::TempDir;

use pi_coding_agent::autoresearch::session::{MetricDirection, Session, SessionConfig};

// ── helpers ───────────────────────────────────────────────────────────────────

fn lower_config(name: &str) -> SessionConfig {
    SessionConfig {
        name: name.to_string(),
        metric: "latency".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: Some(10),
        working_dir: None,
    }
}

fn higher_config(name: &str) -> SessionConfig {
    SessionConfig {
        name: name.to_string(),
        metric: "throughput".to_string(),
        unit: "ops/s".to_string(),
        direction: MetricDirection::Higher,
        max_iterations: None,
        working_dir: None,
    }
}

// ── Session::load round-trip ──────────────────────────────────────────────────

#[test]
fn session_load_roundtrip_lower() {
    let dir = TempDir::new().unwrap();
    let cfg = lower_config("roundtrip-lower");
    let session = Session::new(dir.path(), cfg.clone());
    session.save_config().unwrap();

    let loaded = Session::load(dir.path()).unwrap();
    assert_eq!(loaded.config.name, cfg.name);
    assert_eq!(loaded.config.metric, cfg.metric);
    assert_eq!(loaded.config.unit, cfg.unit);
    assert_eq!(loaded.config.direction, MetricDirection::Lower);
    assert_eq!(loaded.config.max_iterations, Some(10));
    assert_eq!(loaded.root, dir.path());
}

#[test]
fn session_load_roundtrip_higher() {
    let dir = TempDir::new().unwrap();
    let cfg = higher_config("roundtrip-higher");
    let session = Session::new(dir.path(), cfg.clone());
    session.save_config().unwrap();

    let loaded = Session::load(dir.path()).unwrap();
    assert_eq!(loaded.config.direction, MetricDirection::Higher);
    assert_eq!(loaded.config.max_iterations, None);
}

#[test]
fn session_load_roundtrip_with_working_dir() {
    let dir = TempDir::new().unwrap();
    let wd_dir = TempDir::new().unwrap();
    let mut cfg = lower_config("with-wd");
    cfg.working_dir = Some(wd_dir.path().to_path_buf());

    let session = Session::new(dir.path(), cfg.clone());
    session.save_config().unwrap();

    let loaded = Session::load(dir.path()).unwrap();
    assert_eq!(
        loaded.config.working_dir.as_deref(),
        cfg.working_dir.as_deref()
    );
}

// ── Session::load: missing config file → Err ─────────────────────────────────

#[test]
fn session_load_missing_config_gives_not_found() {
    let dir = TempDir::new().unwrap();
    let result = Session::load(dir.path());
    assert!(result.is_err(), "missing config.json should give Err");
    let err = result.err().unwrap();
    assert_eq!(
        err.kind(),
        io::ErrorKind::NotFound,
        "missing config.json should give NotFound; got: {err:?}"
    );
}

// ── Session::load: invalid JSON → Err(InvalidData) ───────────────────────────

#[test]
fn session_load_invalid_json_gives_invalid_data() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("autoresearch.config.json"), b"not-json").unwrap();
    let result = Session::load(dir.path());
    assert!(result.is_err(), "invalid JSON should give Err");
    let err = result.err().unwrap();
    assert_eq!(
        err.kind(),
        io::ErrorKind::InvalidData,
        "invalid JSON should give InvalidData; got: {err:?}"
    );
}

// ── Path helpers return expected files ────────────────────────────────────────

#[test]
fn session_config_path_correct() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("paths"));
    assert_eq!(
        session.config_path(),
        dir.path().join("autoresearch.config.json")
    );
}

#[test]
fn session_jsonl_path_correct() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("paths"));
    assert_eq!(
        session.jsonl_path(),
        dir.path().join("autoresearch.jsonl")
    );
}

#[test]
fn session_md_path_correct() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("paths"));
    assert_eq!(
        session.md_path(),
        dir.path().join("autoresearch.md")
    );
}

#[test]
fn session_checks_script_path_correct() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("paths"));
    assert_eq!(
        session.checks_script(),
        dir.path().join("autoresearch.checks.sh")
    );
}

#[test]
fn session_benchmark_script_path_correct() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("paths"));
    assert_eq!(
        session.benchmark_script(),
        dir.path().join("autoresearch.sh")
    );
}

// ── save_md: Lower direction label ────────────────────────────────────────────

#[test]
fn save_md_lower_direction_label() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("lower-label"));
    session.save_md().unwrap();
    let md = std::fs::read_to_string(session.md_path()).unwrap();
    assert!(md.contains("lower is better"), "lower direction should say 'lower is better'; got:\n{md}");
}

// ── save_md: Higher direction label ───────────────────────────────────────────

#[test]
fn save_md_higher_direction_label() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), higher_config("higher-label"));
    session.save_md().unwrap();
    let md = std::fs::read_to_string(session.md_path()).unwrap();
    assert!(md.contains("higher is better"), "higher direction should say 'higher is better'; got:\n{md}");
}

// ── save_md: unlimited max_iterations ────────────────────────────────────────

#[test]
fn save_md_unlimited_max_iterations() {
    let dir = TempDir::new().unwrap();
    let mut cfg = lower_config("unlimited");
    cfg.max_iterations = None;
    let session = Session::new(dir.path(), cfg);
    session.save_md().unwrap();
    let md = std::fs::read_to_string(session.md_path()).unwrap();
    assert!(md.contains("unlimited"), "None max_iterations should render as 'unlimited'; got:\n{md}");
}

// ── save_md: specific max_iterations ─────────────────────────────────────────

#[test]
fn save_md_specific_max_iterations() {
    let dir = TempDir::new().unwrap();
    let mut cfg = lower_config("capped");
    cfg.max_iterations = Some(42);
    let session = Session::new(dir.path(), cfg);
    session.save_md().unwrap();
    let md = std::fs::read_to_string(session.md_path()).unwrap();
    assert!(md.contains("42"), "max_iterations=42 should appear in md; got:\n{md}");
}

// ── save_md: custom working_dir in markdown ────────────────────────────────────

#[test]
fn save_md_custom_working_dir_appears() {
    let dir = TempDir::new().unwrap();
    let wd = TempDir::new().unwrap();
    let mut cfg = lower_config("custom-wd");
    cfg.working_dir = Some(wd.path().to_path_buf());
    let session = Session::new(dir.path(), cfg);
    session.save_md().unwrap();
    let md = std::fs::read_to_string(session.md_path()).unwrap();
    let wd_str = wd.path().to_str().unwrap();
    assert!(
        md.contains(wd_str),
        "custom working_dir should appear in md; got:\n{md}"
    );
}

// ── save_md: no working_dir uses root in markdown ─────────────────────────────

#[test]
fn save_md_no_working_dir_uses_root() {
    let dir = TempDir::new().unwrap();
    let session = Session::new(dir.path(), lower_config("no-wd"));
    session.save_md().unwrap();
    let md = std::fs::read_to_string(session.md_path()).unwrap();
    let root_str = dir.path().to_str().unwrap();
    assert!(
        md.contains(root_str),
        "root dir should appear in md when no working_dir; got:\n{md}"
    );
}

// ── Session::new + save_config creates readable config ───────────────────────

#[test]
fn session_save_config_creates_valid_json() {
    let dir = TempDir::new().unwrap();
    let cfg = lower_config("json-check");
    let session = Session::new(dir.path(), cfg);
    session.save_config().unwrap();

    let raw = std::fs::read_to_string(session.config_path()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).expect("config must be valid JSON");
    assert_eq!(v["name"], "json-check");
    assert_eq!(v["metric"], "latency");
    assert_eq!(v["direction"], "lower");
}
