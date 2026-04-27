//! Integration tests for pi-autoresearch core (Step 6).
//!
//! Covers:
//! - Session::new + save_md writes a markdown header
//! - Config round-trip via save_config / load
//! - JsonlLog: append → read_all preserves insertion order and entry kinds
//! - count_kept_results counts only Result entries with kept=true
//! - best_result honours Lower / Higher direction
//! - parse_metric: parses `METRIC name=42.5` from stdout
//! - parse_metric: returns None when no METRIC line is present

use pi_coding_agent::autoresearch::{
    log::{JsonlLog, LogEntryKind},
    session::{MetricDirection, Session, SessionConfig},
    tools::InitExperimentTool,
};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

fn sample_config(name: &str, direction: MetricDirection) -> SessionConfig {
    SessionConfig {
        name: name.to_string(),
        metric: "latency".to_string(),
        unit: "ms".to_string(),
        direction,
        max_iterations: Some(10),
        working_dir: None,
    }
}

fn lower_log(dir: &TempDir) -> JsonlLog {
    JsonlLog::new(dir.path().join("test.jsonl"), MetricDirection::Lower)
}

fn higher_log(dir: &TempDir) -> JsonlLog {
    JsonlLog::new(dir.path().join("test.jsonl"), MetricDirection::Higher)
}

// ── Session::new + save_md ────────────────────────────────────────────────────

#[test]
fn session_new_save_md_writes_header() {
    let dir = TempDir::new().unwrap();
    let cfg = sample_config("test-experiment", MetricDirection::Lower);
    let session = Session::new(dir.path(), cfg);
    session.save_md().unwrap();

    let md = std::fs::read_to_string(session.md_path()).unwrap();
    assert!(
        md.contains("# autoresearch: test-experiment"),
        "markdown must contain the experiment name header; got:\n{}",
        md
    );
    assert!(md.contains("latency"), "markdown must contain metric name");
    assert!(md.contains("lower is better"), "markdown must mention direction");
    assert!(
        md.contains("autoresearch.jsonl"),
        "markdown must list the log file"
    );
}

// ── Config round-trip ─────────────────────────────────────────────────────────

#[test]
fn session_config_roundtrip() {
    let dir = TempDir::new().unwrap();
    let cfg = sample_config("roundtrip", MetricDirection::Higher);
    let session = Session::new(dir.path(), cfg.clone());
    session.save_config().unwrap();

    let loaded = Session::load(dir.path()).unwrap();
    assert_eq!(loaded.config.name, cfg.name);
    assert_eq!(loaded.config.metric, cfg.metric);
    assert_eq!(loaded.config.unit, cfg.unit);
    assert!(
        matches!(loaded.config.direction, MetricDirection::Higher),
        "direction must survive round-trip"
    );
    assert_eq!(loaded.config.max_iterations, cfg.max_iterations);
}

// ── JsonlLog: append → read_all preserves order + kinds ──────────────────────

#[test]
fn jsonl_append_read_all_order_and_kinds() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    let cfg = sample_config("order-test", MetricDirection::Lower);

    let e0 = log
        .append(LogEntryKind::Init { config: cfg.clone() })
        .unwrap();
    let e1 = log
        .append(LogEntryKind::Run {
            idea: "idea A".into(),
            commit_before: "abc1234".into(),
        })
        .unwrap();
    let e2 = log
        .append(LogEntryKind::Result {
            run_id: e1.id.clone(),
            metric_value: 42.0,
            duration_ms: 100,
            kept: true,
            commit_after: "def5678".into(),
            checks_passed: true,
        })
        .unwrap();

    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 3, "must have exactly 3 entries");

    // Order preservation
    assert_eq!(entries[0].id, e0.id);
    assert_eq!(entries[1].id, e1.id);
    assert_eq!(entries[2].id, e2.id);

    // Kind checks
    assert!(matches!(entries[0].kind, LogEntryKind::Init { .. }));
    assert!(matches!(entries[1].kind, LogEntryKind::Run { .. }));
    assert!(matches!(entries[2].kind, LogEntryKind::Result { .. }));
}

// ── count_kept_results ────────────────────────────────────────────────────────

#[test]
fn count_kept_results_only_counts_kept_true() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);

    let run = log
        .append(LogEntryKind::Run {
            idea: "x".into(),
            commit_before: "aaa".into(),
        })
        .unwrap();

    // kept=true
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 10.0,
        duration_ms: 50,
        kept: true,
        commit_after: "bbb".into(),
        checks_passed: true,
    })
    .unwrap();
    // kept=false
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 20.0,
        duration_ms: 60,
        kept: false,
        commit_after: "ccc".into(),
        checks_passed: false,
    })
    .unwrap();
    // kept=true again
    log.append(LogEntryKind::Result {
        run_id: run.id.clone(),
        metric_value: 8.0,
        duration_ms: 45,
        kept: true,
        commit_after: "ddd".into(),
        checks_passed: true,
    })
    .unwrap();

    let count = log.count_kept_results().unwrap();
    assert_eq!(count, 2, "should count exactly 2 kept=true results");
}

// ── best_result honours Lower direction ──────────────────────────────────────

#[test]
fn best_result_lower_returns_minimum() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);

    let run = log
        .append(LogEntryKind::Run {
            idea: "y".into(),
            commit_before: "000".into(),
        })
        .unwrap();

    for (v, kept) in [(100.0, true), (50.0, true), (200.0, false), (30.0, true)] {
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

    let best = log.best_result().unwrap();
    assert_eq!(
        best,
        Some(30.0),
        "Lower direction: best must be the minimum of kept values"
    );
}

// ── best_result honours Higher direction ─────────────────────────────────────

#[test]
fn best_result_higher_returns_maximum() {
    let dir = TempDir::new().unwrap();
    let log = higher_log(&dir);

    let run = log
        .append(LogEntryKind::Run {
            idea: "z".into(),
            commit_before: "111".into(),
        })
        .unwrap();

    for (v, kept) in [(100.0, true), (50.0, false), (200.0, true), (30.0, true)] {
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

    let best = log.best_result().unwrap();
    assert_eq!(
        best,
        Some(200.0),
        "Higher direction: best must be the maximum of kept values"
    );
}

// ── best_result returns None on empty log ────────────────────────────────────

#[test]
fn best_result_empty_returns_none() {
    let dir = TempDir::new().unwrap();
    let log = lower_log(&dir);
    assert_eq!(log.best_result().unwrap(), None);
}

// ── parse_metric: success ─────────────────────────────────────────────────────

#[test]
fn parse_metric_parses_metric_line() {
    use pi_coding_agent::autoresearch::tools::parse_metric;

    let stdout = "compiling...\nMETRIC latency=42.5\ndone.\n";
    let value = parse_metric(stdout, "latency");
    assert_eq!(
        value,
        Some(42.5),
        "must parse the numeric value from `METRIC name=value` line"
    );
}

#[test]
fn parse_metric_handles_integer_value() {
    use pi_coding_agent::autoresearch::tools::parse_metric;

    let stdout = "METRIC throughput=1000\n";
    assert_eq!(parse_metric(stdout, "throughput"), Some(1000.0));
}

#[test]
fn parse_metric_first_match_wins() {
    use pi_coding_agent::autoresearch::tools::parse_metric;

    let stdout = "METRIC speed=1.0\nMETRIC speed=2.0\n";
    assert_eq!(
        parse_metric(stdout, "speed"),
        Some(1.0),
        "first matching line takes precedence"
    );
}

// ── parse_metric: failure ─────────────────────────────────────────────────────

#[test]
fn parse_metric_returns_none_when_absent() {
    use pi_coding_agent::autoresearch::tools::parse_metric;

    let stdout = "no metrics here\nsome other output\n";
    let value = parse_metric(stdout, "latency");
    assert_eq!(
        value, None,
        "must return None when no METRIC line is present"
    );
}

#[test]
fn parse_metric_returns_none_for_wrong_metric_name() {
    use pi_coding_agent::autoresearch::tools::parse_metric;

    let stdout = "METRIC throughput=99.9\n";
    assert_eq!(
        parse_metric(stdout, "latency"),
        None,
        "must not match a different metric name"
    );
}

// ── InitExperimentTool smoke test ─────────────────────────────────────────────

#[test]
fn init_tool_creates_config_and_md() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap().to_string();

    let tool = InitExperimentTool;
    let ctx = ToolContext {
        cwd: dir.path().to_path_buf(),
        ..Default::default()
    };
    let input = json!({
        "root": root,
        "name": "smoke-test",
        "metric": "fps",
        "unit": "frames/s",
        "direction": "higher"
    });

    let result = tokio_test::block_on(tool.invoke(&ctx, "test-call-id", input)).unwrap();
    assert!(!result.is_error, "tool must succeed; output: {}", result.model_output);

    // config file written?
    let config_path = dir.path().join("autoresearch.config.json");
    assert!(config_path.exists(), "autoresearch.config.json must be created");

    // md file written?
    let md_path = dir.path().join("autoresearch.md");
    assert!(md_path.exists(), "autoresearch.md must be created");

    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.contains("# autoresearch: smoke-test"), "md must contain experiment name header");

    // jsonl log has Init entry?
    let log = JsonlLog::new(dir.path().join("autoresearch.jsonl"), MetricDirection::Higher);
    let entries = log.read_all().unwrap();
    assert_eq!(entries.len(), 1, "log must have exactly one Init entry");
    assert!(matches!(entries[0].kind, LogEntryKind::Init { .. }));
}
