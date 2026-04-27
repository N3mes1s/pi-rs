//! Tests for `autoresearch::confidence`.

use pi_coding_agent::autoresearch::{
    compute_confidence, ConfidenceBand,
    session::MetricDirection,
};

// ── Insufficient (< 3 samples) ────────────────────────────────────────────────

#[test]
fn insufficient_when_zero_samples() {
    let s = compute_confidence(&[], 100.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Insufficient);
    assert_eq!(s.multiplier, 0.0);
}

#[test]
fn insufficient_when_one_sample() {
    let s = compute_confidence(&[90.0], 100.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Insufficient);
}

#[test]
fn insufficient_when_two_samples() {
    let s = compute_confidence(&[90.0, 80.0], 100.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Insufficient);
}

#[test]
fn sufficient_with_three_samples() {
    // Just assert that we do NOT get Insufficient with 3 samples.
    let s = compute_confidence(&[90.0, 95.0, 85.0], 100.0, MetricDirection::Lower);
    assert_ne!(s.band, ConfidenceBand::Insufficient);
}

// ── Known MAD calculation ─────────────────────────────────────────────────────

/// Samples: [1, 2, 3, 4, 5]
/// Sorted:  [1, 2, 3, 4, 5]  → median = 3
/// Deviations: |1-3|=2, |2-3|=1, |3-3|=0, |4-3|=1, |5-3|=2
/// Sorted deviations: [0, 1, 1, 2, 2] → MAD = 1
/// Baseline = 10, direction = Lower → best = 10 − 1 = 9
/// multiplier = 9 / 1 = 9.0 → Green
#[test]
fn mad_on_known_sample_lower() {
    let samples = [1.0, 2.0, 3.0, 4.0, 5.0];
    let s = compute_confidence(&samples, 10.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green, "multiplier = {}", s.multiplier);
    // MAD = 1, improvement = 9 → multiplier = 9.0
    assert!((s.multiplier - 9.0).abs() < 1e-9, "expected 9.0, got {}", s.multiplier);
}

/// Same samples with direction = Higher.
/// best = max(samples) = 5, baseline = 1
/// improvement = 5 − 1 = 4, MAD = 1
/// multiplier = 4.0 → Green
#[test]
fn mad_on_known_sample_higher() {
    let samples = [1.0, 2.0, 3.0, 4.0, 5.0];
    let s = compute_confidence(&samples, 1.0, MetricDirection::Higher);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!((s.multiplier - 4.0).abs() < 1e-9, "expected 4.0, got {}", s.multiplier);
}

// ── Band thresholds ───────────────────────────────────────────────────────────

/// Force multiplier < 1.0 → Red.
/// Samples all very close to baseline: [98, 99, 100]
/// median = 99, deviations = [1, 0, 1] → sorted [0, 1, 1] → MAD = 1
/// Lower direction: best = 100 - 98 = 2, multiplier = 2/1 = 2.0 → Green
/// (adjust baseline so improvement is small)
///
/// Let samples = [100, 100, 100] with a tiny improvement.
/// All samples identical → MAD = 0.
/// best_improvement = 100 - 100 = 0 → multiplier = 0 → Red.
#[test]
fn band_red_when_no_improvement() {
    let samples = [100.0, 100.0, 100.0];
    let s = compute_confidence(&samples, 100.0, MetricDirection::Lower);
    // MAD=0, improvement=0 → multiplier=0 → Red
    assert_eq!(s.band, ConfidenceBand::Red);
    assert_eq!(s.multiplier, 0.0);
}

/// multiplier exactly 1.0 → Yellow.
/// samples = [10, 20, 30], median=20, deviations=[10,0,10], MAD=10
/// Lower, baseline=30: improvement = 30-10 = 20, multiplier = 20/10 = 2.0 → Green
///
/// To get exactly Yellow: want multiplier in [1.0, 2.0).
/// samples = [10, 20, 30], MAD=10
/// Lower, baseline=25: improvement = 25-10 = 15, mult = 1.5 → Yellow
#[test]
fn band_yellow_between_1_and_2() {
    let samples = [10.0, 20.0, 30.0];
    // MAD = 10 (median=20, deviations=[10,0,10])
    let s = compute_confidence(&samples, 25.0, MetricDirection::Lower);
    // improvement = 25 - 10 = 15, mult = 15/10 = 1.5 → Yellow
    assert_eq!(s.band, ConfidenceBand::Yellow, "multiplier = {}", s.multiplier);
    assert!((s.multiplier - 1.5).abs() < 1e-9);
}

/// multiplier ≥ 2.0 → Green.
#[test]
fn band_green_at_exactly_2() {
    let samples = [10.0, 20.0, 30.0];
    // MAD=10, Lower, baseline=30: improvement=20, mult=2.0 → Green
    let s = compute_confidence(&samples, 30.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!((s.multiplier - 2.0).abs() < 1e-9);
}

// ── Lower vs Higher direction ─────────────────────────────────────────────────

/// With Lower direction: improvement = baseline − min(samples).
/// If min(samples) > baseline → improvement is negative → multiplier = 0 → Red.
#[test]
fn lower_direction_regression_gives_red() {
    let samples = [110.0, 120.0, 130.0]; // all worse than baseline
    let s = compute_confidence(&samples, 100.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Red);
    assert_eq!(s.multiplier, 0.0);
}

/// With Higher direction: improvement = max(samples) − baseline.
/// If max(samples) < baseline → improvement negative → multiplier = 0 → Red.
#[test]
fn higher_direction_regression_gives_red() {
    let samples = [10.0, 20.0, 30.0]; // all below baseline of 50
    let s = compute_confidence(&samples, 50.0, MetricDirection::Higher);
    assert_eq!(s.band, ConfidenceBand::Red);
    assert_eq!(s.multiplier, 0.0);
}

// ── Zero MAD with improvement ─────────────────────────────────────────────────

/// All samples identical but below baseline → infinite multiplier → Green.
#[test]
fn zero_mad_with_improvement_is_green() {
    let samples = [50.0, 50.0, 50.0];
    let s = compute_confidence(&samples, 100.0, MetricDirection::Lower);
    // MAD=0, improvement=50 → infinite → Green
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(s.multiplier.is_infinite());
}

// ── Even-length sample median ─────────────────────────────────────────────────

/// 4 samples: [1, 2, 3, 4] → median = (2+3)/2 = 2.5
/// deviations = [1.5, 0.5, 0.5, 1.5] → sorted = [0.5, 0.5, 1.5, 1.5] → MAD = 1.0
/// Lower, baseline=10: improvement = 10-1 = 9, mult = 9.0 → Green
#[test]
fn even_sample_count_median() {
    let samples = [1.0, 2.0, 3.0, 4.0];
    let s = compute_confidence(&samples, 10.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!((s.multiplier - 9.0).abs() < 1e-9, "multiplier = {}", s.multiplier);
}
