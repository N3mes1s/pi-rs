//! Tests for `autoresearch::dashboard`.

use pi_coding_agent::autoresearch::{
    confidence::{ConfidenceBand, ConfidenceScore},
    dashboard::{render_inline, render_table, DashboardState},
    session::MetricDirection,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn green_score(mult: f64) -> ConfidenceScore {
    ConfidenceScore {
        multiplier: mult,
        band: ConfidenceBand::Green,
    }
}

fn insufficient_score() -> ConfidenceScore {
    ConfidenceScore {
        multiplier: 0.0,
        band: ConfidenceBand::Insufficient,
    }
}

fn sample_state() -> DashboardState {
    DashboardState {
        session_name: "my-experiment".to_string(),
        runs: 12,
        kept: 8,
        metric_name: "total_µs".to_string(),
        baseline: 17_300.0,
        current_best: 15_200.0,
        direction: MetricDirection::Lower,
        confidence: green_score(2.1),
    }
}

// ── render_inline ─────────────────────────────────────────────────────────────

#[test]
fn inline_contains_autoresearch_label() {
    let s = render_inline(&sample_state());
    assert!(s.contains("autoresearch"), "got: {s}");
}

#[test]
fn inline_contains_run_count() {
    let s = render_inline(&sample_state());
    assert!(s.contains("12"), "should contain 12 runs; got: {s}");
    assert!(s.contains("runs"), "got: {s}");
}

#[test]
fn inline_contains_kept_count() {
    let s = render_inline(&sample_state());
    assert!(s.contains("8"), "should contain 8 kept; got: {s}");
    assert!(s.contains("kept"), "got: {s}");
}

#[test]
fn inline_contains_metric_name() {
    let s = render_inline(&sample_state());
    assert!(s.contains("total_µs"), "should contain metric name; got: {s}");
}

#[test]
fn inline_contains_best_value() {
    let s = render_inline(&sample_state());
    // 15_200 formatted with comma separator → "15,200"
    assert!(s.contains("15,200"), "should contain formatted best value; got: {s}");
}

#[test]
fn inline_contains_percent_change() {
    let s = render_inline(&sample_state());
    // baseline=17300, best=15200, Lower → pct = (15200-17300)/17300 ≈ -12.1%
    // Just check for a percent sign and a minus sign.
    assert!(s.contains('%'), "should contain percent sign; got: {s}");
    assert!(s.contains('-'), "should contain minus for improvement; got: {s}");
}

#[test]
fn inline_contains_confidence_multiplier() {
    let s = render_inline(&sample_state());
    // 2.1×
    assert!(s.contains("2.1"), "should contain multiplier 2.1; got: {s}");
    assert!(s.contains('×'), "should contain × symbol; got: {s}");
}

#[test]
fn inline_contains_conf_label() {
    let s = render_inline(&sample_state());
    assert!(s.contains("conf"), "should contain 'conf'; got: {s}");
}

// ── render_inline with zero baseline ─────────────────────────────────────────

#[test]
fn inline_zero_baseline_no_panic() {
    let state = DashboardState {
        session_name: "zero-baseline".to_string(),
        runs: 3,
        kept: 1,
        metric_name: "ops".to_string(),
        baseline: 0.0,
        current_best: 500.0,
        direction: MetricDirection::Higher,
        confidence: insufficient_score(),
    };
    // Should not panic; percent change returns 0.0 for zero baseline.
    let s = render_inline(&state);
    assert!(s.contains("autoresearch"), "got: {s}");
    assert!(s.contains("0.0%"), "zero baseline → 0.0%; got: {s}");
}

// ── render_table ──────────────────────────────────────────────────────────────

fn sample_runs() -> Vec<(String, f64, bool)> {
    vec![
        ("try cache".to_string(), 16_000.0, true),
        ("vectorise loop".to_string(), 18_500.0, false),
        ("remove allocation".to_string(), 15_200.0, true),
    ]
}

#[test]
fn table_contains_header() {
    let state = sample_state();
    let runs = sample_runs();
    let t = render_table(&state, &runs);
    assert!(t.contains("autoresearch"), "got: {t}");
    assert!(t.contains("my-experiment"), "got: {t}");
}

#[test]
fn table_contains_all_ideas() {
    let state = sample_state();
    let runs = sample_runs();
    let t = render_table(&state, &runs);
    assert!(t.contains("try cache"), "got: {t}");
    assert!(t.contains("vectorise loop"), "got: {t}");
    assert!(t.contains("remove allocation"), "got: {t}");
}

#[test]
fn table_marks_kept_and_not_kept() {
    let state = sample_state();
    let runs = sample_runs();
    let t = render_table(&state, &runs);
    // Two kept (✓) and one not-kept (✗).
    let kept_count = t.matches('✓').count();
    let rej_count = t.matches('✗').count();
    assert_eq!(kept_count, 2, "expected 2 kept marks; got: {t}");
    assert_eq!(rej_count, 1, "expected 1 rejected mark; got: {t}");
}

#[test]
fn table_contains_metric_values() {
    let state = sample_state();
    let runs = sample_runs();
    let t = render_table(&state, &runs);
    // 16,000 and 15,200 should appear with comma formatting.
    assert!(t.contains("16,000") || t.contains("16000"), "got: {t}");
    assert!(t.contains("15,200") || t.contains("15200"), "got: {t}");
}

#[test]
fn table_with_empty_runs_shows_header() {
    let state = DashboardState {
        session_name: "empty".to_string(),
        runs: 0,
        kept: 0,
        metric_name: "ms".to_string(),
        baseline: 100.0,
        current_best: 100.0,
        direction: MetricDirection::Lower,
        confidence: insufficient_score(),
    };
    let t = render_table(&state, &[]);
    assert!(t.contains("empty"), "got: {t}");
    // No footer when empty.
    assert!(!t.contains("best improvement"), "got: {t}");
}

#[test]
fn table_contains_direction() {
    let state = sample_state();
    let runs = sample_runs();
    let t = render_table(&state, &runs);
    assert!(t.contains("lower"), "direction should appear in table; got: {t}");
}

#[test]
fn table_contains_baseline() {
    let state = sample_state();
    let runs = sample_runs();
    let t = render_table(&state, &runs);
    // baseline = 17,300
    assert!(t.contains("17,300") || t.contains("17300"), "baseline should appear; got: {t}");
}

// ── percent formatting edge cases ─────────────────────────────────────────────

#[test]
fn inline_higher_direction_positive_pct() {
    let state = DashboardState {
        session_name: "throughput".to_string(),
        runs: 5,
        kept: 3,
        metric_name: "ops".to_string(),
        baseline: 1000.0,
        current_best: 1200.0,
        direction: MetricDirection::Higher,
        confidence: green_score(3.0),
    };
    let s = render_inline(&state);
    // improvement = (1200-1000)/1000 = +20%
    assert!(s.contains('+'), "higher direction improvement should be positive; got: {s}");
}
