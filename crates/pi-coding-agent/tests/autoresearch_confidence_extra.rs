//! Extra coverage for autoresearch::confidence.
//!
//! Covers:
//! - even-length median (already in autoresearch_confidence.rs but more variants)
//! - all-equal samples (MAD=0): no improvement → Red (multiplier=0.0)
//! - all-equal samples (MAD=0): with improvement → Green (multiplier=∞)
//! - negative direction (Higher) with all-decreasing values → Red
//! - exactly 3 samples is sufficient (boundary)
//! - large improvement / small MAD → very large multiplier

use pi_coding_agent::autoresearch::{compute_confidence, session::MetricDirection, ConfidenceBand};

// ── All-equal samples: MAD=0, no improvement → Red ───────────────────────────

#[test]
fn all_equal_samples_no_improvement_gives_red() {
    // All samples same as baseline → improvement = 0, MAD = 0 → Red
    let samples = [75.0, 75.0, 75.0, 75.0];
    let s = compute_confidence(&samples, 75.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Red);
    assert_eq!(s.multiplier, 0.0);
}

// ── All-equal samples: MAD=0, improvement > 0 → Green (∞) ────────────────────

#[test]
fn all_equal_samples_with_improvement_gives_green_infinity() {
    // All samples lower than baseline (Lower direction) → improvement > 0, MAD = 0 → ∞ → Green
    let samples = [50.0, 50.0, 50.0];
    let s = compute_confidence(&samples, 100.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(
        s.multiplier.is_infinite(),
        "expected ∞ multiplier; got {}",
        s.multiplier
    );
}

// ── All-equal samples Higher: MAD=0, improvement → Green (∞) ─────────────────

#[test]
fn all_equal_samples_higher_with_improvement_green_infinity() {
    // All samples higher than baseline (Higher direction) → ∞ → Green
    let samples = [200.0, 200.0, 200.0];
    let s = compute_confidence(&samples, 100.0, MetricDirection::Higher);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(s.multiplier.is_infinite());
}

// ── Negative direction (Higher): all-decreasing → Red ────────────────────────

#[test]
fn higher_direction_all_decreasing_gives_red() {
    // All samples below baseline with Higher direction → improvement < 0 → clamped to 0 → Red
    let samples = [10.0, 20.0, 30.0]; // all below baseline of 100
    let s = compute_confidence(&samples, 100.0, MetricDirection::Higher);
    assert_eq!(s.band, ConfidenceBand::Red);
    assert_eq!(s.multiplier, 0.0);
}

// ── Exactly 3 samples is sufficient ──────────────────────────────────────────

#[test]
fn exactly_three_samples_not_insufficient() {
    let samples = [1.0, 2.0, 3.0];
    let s = compute_confidence(&samples, 10.0, MetricDirection::Lower);
    assert_ne!(
        s.band,
        ConfidenceBand::Insufficient,
        "3 samples should be sufficient"
    );
}

// ── Exactly 2 samples is insufficient ────────────────────────────────────────

#[test]
fn exactly_two_samples_insufficient() {
    let samples = [1.0, 2.0];
    let s = compute_confidence(&samples, 10.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Insufficient);
}

// ── Even-length median: 6 samples ────────────────────────────────────────────

/// 6 samples: [1, 2, 3, 4, 5, 6] → sorted → median = (3+4)/2 = 3.5
/// deviations = [2.5, 1.5, 0.5, 0.5, 1.5, 2.5] → sorted = [0.5, 0.5, 1.5, 1.5, 2.5, 2.5]
/// MAD = (1.5+1.5)/2 = 1.5
/// Lower, baseline=10: best = 10 - 1 = 9, mult = 9/1.5 = 6.0 → Green
#[test]
fn even_six_samples_median() {
    let samples = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let s = compute_confidence(&samples, 10.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(
        (s.multiplier - 6.0).abs() < 1e-9,
        "expected 6.0, got {}",
        s.multiplier
    );
}

// ── Large improvement / small MAD → high multiplier ──────────────────────────

#[test]
fn large_improvement_small_mad_very_high_multiplier() {
    // Samples very close together but far below baseline.
    // [99.9, 100.0, 100.1] → median = 100.0, deviations = [0.1, 0.0, 0.1] → MAD = 0.1
    // Lower, baseline = 1000: improvement = 1000 - 99.9 = 900.1, mult = 900.1/0.1 = 9001
    let samples = [99.9, 100.0, 100.1];
    let s = compute_confidence(&samples, 1000.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(
        s.multiplier > 100.0,
        "expected very high multiplier; got {}",
        s.multiplier
    );
}

// ── Higher direction: improvement is max(samples) − baseline ─────────────────

#[test]
fn higher_direction_improvement_is_max_minus_baseline() {
    // samples = [5, 10, 15], baseline = 0, Higher
    // max = 15, improvement = 15, MAD calculation:
    // sorted = [5, 10, 15], median = 10
    // deviations = [5, 0, 5], MAD = 5
    // mult = 15/5 = 3.0 → Green
    let samples = [5.0, 10.0, 15.0];
    let s = compute_confidence(&samples, 0.0, MetricDirection::Higher);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(
        (s.multiplier - 3.0).abs() < 1e-9,
        "expected 3.0, got {}",
        s.multiplier
    );
}

// ── Odd-length median: 5 samples ─────────────────────────────────────────────

/// Verify median_of_sorted with odd length > 1.
/// [2, 4, 6, 8, 10] → median = 6
/// deviations = [4, 2, 0, 2, 4] → sorted → MAD = 2
/// Lower, baseline=20: improvement = 20-2 = 18, mult = 18/2 = 9 → Green
#[test]
fn odd_five_samples_median() {
    let samples = [2.0, 4.0, 6.0, 8.0, 10.0];
    let s = compute_confidence(&samples, 20.0, MetricDirection::Lower);
    assert_eq!(s.band, ConfidenceBand::Green);
    assert!(
        (s.multiplier - 9.0).abs() < 1e-9,
        "expected 9.0, got {}",
        s.multiplier
    );
}

// ── multiplier clamped to 0 when improvement is negative ─────────────────────

#[test]
fn negative_improvement_multiplier_clamped_to_zero() {
    // Lower direction: min(samples) > baseline → negative improvement → clamped to 0
    let samples = [110.0, 120.0, 130.0];
    let s = compute_confidence(&samples, 100.0, MetricDirection::Lower);
    assert_eq!(s.multiplier, 0.0);
    assert_eq!(s.band, ConfidenceBand::Red);
}

// ── Unsorted input is sorted before median ────────────────────────────────────

#[test]
fn unsorted_input_gives_same_result_as_sorted() {
    let sorted = [1.0, 2.0, 3.0, 4.0, 5.0];
    let unsorted = [5.0, 1.0, 3.0, 2.0, 4.0];
    let s1 = compute_confidence(&sorted, 10.0, MetricDirection::Lower);
    let s2 = compute_confidence(&unsorted, 10.0, MetricDirection::Lower);
    assert_eq!(s1.band, s2.band);
    assert!((s1.multiplier - s2.multiplier).abs() < 1e-9);
}
