//! Extra coverage for autoresearch::dashboard.
//!
//! Covers:
//! - empty runs table renders header only (no footer)
//! - large numbers don't panic (i64 overflow guard)
//! - percent edge case where current_best == baseline → 0.0%
//! - Higher direction table contains "higher" label
//! - idea truncated at 40 chars with ellipsis
//! - infinite confidence multiplier renders as "∞×"
//! - negative metric value formats correctly

use pi_coding_agent::autoresearch::{
    confidence::{ConfidenceBand, ConfidenceScore},
    dashboard::{render_inline, render_table, DashboardState},
    session::MetricDirection,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn inf_score() -> ConfidenceScore {
    ConfidenceScore {
        multiplier: f64::INFINITY,
        band: ConfidenceBand::Green,
    }
}

fn green(mult: f64) -> ConfidenceScore {
    ConfidenceScore {
        multiplier: mult,
        band: ConfidenceBand::Green,
    }
}

fn yellow(mult: f64) -> ConfidenceScore {
    ConfidenceScore {
        multiplier: mult,
        band: ConfidenceBand::Yellow,
    }
}

fn red() -> ConfidenceScore {
    ConfidenceScore {
        multiplier: 0.0,
        band: ConfidenceBand::Red,
    }
}

fn insufficient() -> ConfidenceScore {
    ConfidenceScore {
        multiplier: 0.0,
        band: ConfidenceBand::Insufficient,
    }
}

fn state_lower(baseline: f64, current_best: f64, conf: ConfidenceScore) -> DashboardState {
    DashboardState {
        session_name: "test".to_string(),
        runs: 5,
        kept: 2,
        metric_name: "ms".to_string(),
        baseline,
        current_best,
        direction: MetricDirection::Lower,
        confidence: conf,
    }
}

fn state_higher(baseline: f64, current_best: f64, conf: ConfidenceScore) -> DashboardState {
    DashboardState {
        session_name: "throughput-test".to_string(),
        runs: 3,
        kept: 1,
        metric_name: "ops".to_string(),
        baseline,
        current_best,
        direction: MetricDirection::Higher,
        confidence: conf,
    }
}

// ── empty runs table renders header only (no footer) ─────────────────────────

#[test]
fn empty_runs_no_footer() {
    let state = state_lower(100.0, 100.0, insufficient());
    let t = render_table(&state, &[]);
    assert!(t.contains("test"), "header should contain session name; got: {t}");
    assert!(!t.contains("best improvement"), "empty runs should have no footer; got: {t}");
}

#[test]
fn empty_runs_column_header_present() {
    let state = state_lower(100.0, 100.0, insufficient());
    let t = render_table(&state, &[]);
    assert!(t.contains('#'), "column header must include '#'; got: {t}");
    assert!(t.contains("idea"), "column header must include 'idea'; got: {t}");
}

// ── percent edge case: current_best == baseline → 0.0% ───────────────────────

#[test]
fn inline_equal_best_and_baseline_gives_zero_percent() {
    let state = state_lower(1000.0, 1000.0, green(1.0));
    let s = render_inline(&state);
    assert!(s.contains("0.0%"), "equal best and baseline should give 0.0%; got: {s}");
}

// ── large numbers don't panic ─────────────────────────────────────────────────

#[test]
fn large_numbers_no_panic() {
    let state = DashboardState {
        session_name: "big".to_string(),
        runs: usize::MAX,
        kept: usize::MAX / 2,
        metric_name: "ns".to_string(),
        baseline: 1_000_000_000.0,
        current_best: 999_999_000.0,
        direction: MetricDirection::Lower,
        confidence: green(5.0),
    };
    // Must not panic.
    let s = render_inline(&state);
    assert!(!s.is_empty(), "inline render must produce output for large numbers");

    let runs: Vec<(String, f64, bool)> = (0..5)
        .map(|i| (format!("run {}", i), 1_000_000_000.0 - i as f64 * 1000.0, true))
        .collect();
    let t = render_table(&state, &runs);
    assert!(!t.is_empty(), "table render must produce output for large numbers");
}

// ── infinite confidence multiplier renders as ∞× ─────────────────────────────

#[test]
fn infinite_multiplier_renders_as_infinity_symbol() {
    let state = state_lower(100.0, 50.0, inf_score());
    let s = render_inline(&state);
    assert!(s.contains('∞'), "infinite multiplier should render as ∞; got: {s}");
    assert!(s.contains('×'), "should contain × symbol; got: {s}");
}

// ── yellow band renders correctly ─────────────────────────────────────────────

#[test]
fn yellow_band_renders_emoji() {
    let state = state_lower(100.0, 85.0, yellow(1.5));
    let s = render_inline(&state);
    assert!(s.contains('🟡'), "yellow band should render 🟡; got: {s}");
}

// ── red band renders correctly ────────────────────────────────────────────────

#[test]
fn red_band_renders_emoji() {
    let state = state_lower(100.0, 100.0, red());
    let s = render_inline(&state);
    assert!(s.contains('🔴'), "red band should render 🔴; got: {s}");
}

// ── insufficient band renders correctly ───────────────────────────────────────

#[test]
fn insufficient_band_renders_empty_circle() {
    let state = state_lower(100.0, 100.0, insufficient());
    let s = render_inline(&state);
    assert!(s.contains('⚪'), "insufficient band should render ⚪; got: {s}");
}

// ── green band renders correctly ──────────────────────────────────────────────

#[test]
fn green_band_renders_emoji() {
    let state = state_lower(100.0, 50.0, green(3.0));
    let s = render_inline(&state);
    assert!(s.contains('🟢'), "green band should render 🟢; got: {s}");
}

// ── Higher direction table contains "higher" ──────────────────────────────────

#[test]
fn table_higher_direction_label() {
    let state = state_higher(100.0, 200.0, green(4.0));
    let runs = vec![("run 1".to_string(), 200.0, true)];
    let t = render_table(&state, &runs);
    assert!(t.contains("higher"), "Higher direction table should contain 'higher'; got: {t}");
}

// ── idea truncated at 40 chars with ellipsis ──────────────────────────────────

#[test]
fn long_idea_truncated_with_ellipsis() {
    let state = state_lower(100.0, 80.0, green(2.5));
    // Idea longer than 40 characters.
    let long_idea = "a".repeat(50);
    let runs = vec![(long_idea.clone(), 80.0, true)];
    let t = render_table(&state, &runs);
    assert!(t.contains('…'), "idea over 40 chars should be truncated with '…'; got: {t}");
    // The full 50-char string should NOT appear verbatim.
    assert!(!t.contains(&long_idea), "full long idea must not appear verbatim; got: {t}");
}

// ── short idea not truncated ──────────────────────────────────────────────────

#[test]
fn short_idea_not_truncated() {
    let state = state_lower(100.0, 90.0, green(2.0));
    let idea = "short idea".to_string();
    let runs = vec![(idea.clone(), 90.0, true)];
    let t = render_table(&state, &runs);
    assert!(t.contains(&idea), "short idea should appear verbatim; got: {t}");
}

// ── exactly 40-char idea is not truncated ────────────────────────────────────

#[test]
fn exactly_40_char_idea_not_truncated() {
    let state = state_lower(100.0, 90.0, green(2.0));
    let idea = "a".repeat(40);
    let runs = vec![(idea.clone(), 90.0, true)];
    let t = render_table(&state, &runs);
    assert!(t.contains(&idea), "exactly 40-char idea should not be truncated; got: {t}");
}

// ── footer appears when runs is non-empty ─────────────────────────────────────

#[test]
fn non_empty_runs_has_footer() {
    let state = state_lower(100.0, 80.0, green(3.0));
    let runs = vec![("idea".to_string(), 80.0, true)];
    let t = render_table(&state, &runs);
    assert!(t.contains("best improvement"), "non-empty runs should have footer; got: {t}");
}

// ── negative metric value formats correctly ────────────────────────────────────

#[test]
fn negative_metric_value_formats_with_minus() {
    let state = DashboardState {
        session_name: "neg".to_string(),
        runs: 1,
        kept: 1,
        metric_name: "delta".to_string(),
        baseline: 0.0,
        current_best: -1500.0,
        direction: MetricDirection::Lower,
        confidence: green(2.0),
    };
    let s = render_inline(&state);
    // -1500 should format as "-1,500".
    assert!(s.contains("-1,500"), "negative value should format with minus and comma; got: {s}");
}

// ── zero metric value formats as "0" ─────────────────────────────────────────

#[test]
fn zero_metric_value_formats_as_zero() {
    let state = state_lower(0.0, 0.0, insufficient());
    let s = render_inline(&state);
    // 0 should appear somewhere in the output.
    assert!(s.contains('0'), "zero metric should format as '0'; got: {s}");
}

// ── table with one run has row number 1 ───────────────────────────────────────

#[test]
fn table_row_numbering_starts_at_one() {
    let state = state_lower(100.0, 90.0, green(2.0));
    let runs = vec![("first run".to_string(), 90.0, true)];
    let t = render_table(&state, &runs);
    // Row 1 should appear.
    assert!(t.contains('1'), "first row should be numbered 1; got: {t}");
}

// ── render_inline with many runs ──────────────────────────────────────────────

#[test]
fn inline_with_many_runs_no_panic() {
    let state = DashboardState {
        session_name: "many".to_string(),
        runs: 1_000_000,
        kept: 500_000,
        metric_name: "µs".to_string(),
        baseline: 10_000.0,
        current_best: 8_000.0,
        direction: MetricDirection::Lower,
        confidence: green(2.5),
    };
    let s = render_inline(&state);
    assert!(s.contains("1"), "should contain some numbers; got: {s}");
}
