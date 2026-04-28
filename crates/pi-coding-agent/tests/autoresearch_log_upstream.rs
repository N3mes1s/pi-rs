//! Schema-level tests for the upstream-faithful autoresearch JSONL log.

use std::collections::BTreeMap;

use pi_coding_agent::autoresearch::{BestDirection, ConfigEntry, JsonlLog, RunEntry, RunStatus};

fn tmp_log() -> (tempfile::TempDir, JsonlLog) {
    let dir = tempfile::tempdir().unwrap();
    let log = JsonlLog::new(dir.path().join("autoresearch.jsonl"));
    (dir, log)
}

#[test]
fn config_header_serialises_with_upstream_field_names() {
    let (_d, log) = tmp_log();
    let cfg = ConfigEntry::new("opt-startup", "total_us", "µs", BestDirection::Lower);
    log.append_config(&cfg).unwrap();
    let line = std::fs::read_to_string(&log.path).unwrap();
    let v: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(v["type"], "config");
    assert_eq!(v["name"], "opt-startup");
    assert_eq!(v["metricName"], "total_us");
    assert_eq!(v["metricUnit"], "µs");
    assert_eq!(v["bestDirection"], "lower");
}

#[test]
fn run_entry_serialises_with_upstream_field_names() {
    let (_d, log) = tmp_log();
    let mut metrics = BTreeMap::new();
    metrics.insert("size_kib".into(), 5015.0);
    let run = RunEntry {
        run: 1,
        commit: "abc1234".into(),
        metric: 1620.0,
        metrics,
        status: RunStatus::Keep,
        description: "lto=fat".into(),
        timestamp: 1_777_278_226_000,
        confidence: None,
        iteration_tokens: Some(900),
        asi: Some(serde_json::json!({"note": "tested 200x"})),
    };
    log.append_run(&run).unwrap();
    let line = std::fs::read_to_string(&log.path).unwrap();
    let v: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(v["run"], 1);
    assert_eq!(v["commit"], "abc1234");
    assert_eq!(v["metric"], 1620.0);
    assert_eq!(v["metrics"]["size_kib"], 5015.0);
    assert_eq!(v["status"], "keep");
    assert_eq!(v["iterationTokens"], 900);
    assert_eq!(v["asi"]["note"], "tested 200x");
}

#[test]
fn next_run_number_increments() {
    let (_d, log) = tmp_log();
    assert_eq!(log.next_run_number().unwrap(), 1);

    for n in 1..=3 {
        let entry = RunEntry {
            run: n,
            commit: "c".into(),
            metric: n as f64,
            metrics: BTreeMap::new(),
            status: RunStatus::Discard,
            description: format!("idea-{n}"),
            timestamp: 0,
            confidence: None,
            iteration_tokens: None,
            asi: None,
        };
        log.append_run(&entry).unwrap();
    }
    assert_eq!(log.next_run_number().unwrap(), 4);
}

#[test]
fn best_kept_honours_direction() {
    let (_d, log) = tmp_log();
    let push = |run, metric, status: RunStatus| {
        let e = RunEntry {
            run,
            commit: "c".into(),
            metric,
            metrics: BTreeMap::new(),
            status,
            description: "x".into(),
            timestamp: 0,
            confidence: None,
            iteration_tokens: None,
            asi: None,
        };
        log.append_run(&e).unwrap();
    };
    push(1, 100.0, RunStatus::Keep);
    push(2, 90.0, RunStatus::Discard); // ignored
    push(3, 80.0, RunStatus::Keep);
    push(4, 110.0, RunStatus::Keep);

    assert_eq!(log.best_kept(BestDirection::Lower).unwrap(), Some(80.0));
    assert_eq!(log.best_kept(BestDirection::Higher).unwrap(), Some(110.0));
}

#[test]
fn read_latest_config_picks_last_header() {
    let (_d, log) = tmp_log();
    log.append_config(&ConfigEntry::new("v1", "m1", "ms", BestDirection::Lower))
        .unwrap();
    log.append_config(&ConfigEntry::new("v2", "m2", "kb", BestDirection::Higher))
        .unwrap();
    let cfg = log.read_latest_config().unwrap().unwrap();
    assert_eq!(cfg.name.as_deref(), Some("v2"));
    assert_eq!(cfg.metric_name.as_deref(), Some("m2"));
    assert_eq!(cfg.best_direction, Some(BestDirection::Higher));
}

#[test]
fn run_status_parse_handles_all_four_variants() {
    assert_eq!(RunStatus::parse("keep"), Some(RunStatus::Keep));
    assert_eq!(RunStatus::parse("discard"), Some(RunStatus::Discard));
    assert_eq!(RunStatus::parse("crash"), Some(RunStatus::Crash));
    assert_eq!(
        RunStatus::parse("checks_failed"),
        Some(RunStatus::ChecksFailed)
    );
    assert_eq!(RunStatus::parse("unknown"), None);
    assert!(RunStatus::Keep.is_kept());
    assert!(!RunStatus::Discard.is_kept());
}

#[test]
fn parse_metric_handles_int_float_and_µ_in_name() {
    use pi_coding_agent::autoresearch::tools::parse_metric;
    let stdout =
        "warmup\nMETRIC startup_µs=1620\nMETRIC size_kib=5015.5\nMETRIC nope.bad=oops\nbye";
    assert_eq!(parse_metric(stdout, "startup_µs"), Some(1620.0));
    assert_eq!(parse_metric(stdout, "size_kib"), Some(5015.5));
    assert_eq!(parse_metric(stdout, "nope.bad"), None);
    assert_eq!(parse_metric(stdout, "missing"), None);
}
