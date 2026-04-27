//! Confidence scoring for autoresearch experiment runs.
//!
//! [`compute`] derives a [`ConfidenceScore`] from a slice of observed metric
//! samples by computing the Median Absolute Deviation (MAD) and comparing the
//! best observed improvement against that variability.
//!
//! # Bands
//!
//! | Multiplier | Band |
//! |------------|------|
//! | ≥ 2.0 | [`ConfidenceBand::Green`] |
//! | ≥ 1.0 | [`ConfidenceBand::Yellow`] |
//! | < 1.0 | [`ConfidenceBand::Red`] |
//! | < 3 samples | [`ConfidenceBand::Insufficient`] |

use crate::autoresearch::session::MetricDirection;

// ── ConfidenceBand ────────────────────────────────────────────────────────────

/// Qualitative interpretation of a [`ConfidenceScore`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfidenceBand {
    /// Multiplier ≥ 2.0 — strong signal.
    Green,
    /// Multiplier ≥ 1.0 — moderate signal.
    Yellow,
    /// Multiplier < 1.0 — weak or no signal.
    Red,
    /// Fewer than 3 samples — cannot compute a reliable MAD.
    Insufficient,
}

// ── ConfidenceScore ───────────────────────────────────────────────────────────

/// Output of [`compute`].
pub struct ConfidenceScore {
    /// `|best_improvement| / MAD`.  `0.0` when [`ConfidenceBand::Insufficient`].
    pub multiplier: f64,
    /// Qualitative band derived from [`multiplier`].
    pub band: ConfidenceBand,
}

// ── compute ───────────────────────────────────────────────────────────────────

/// Compute a [`ConfidenceScore`] from `samples`.
///
/// * `samples`   — raw metric values observed across experiment runs.
/// * `baseline`  — the pre-experiment reference value.
/// * `direction` — whether *lower* or *higher* values are improvements.
///
/// Returns [`ConfidenceBand::Insufficient`] when `samples.len() < 3`.
///
/// Otherwise:
/// 1. Compute the **median** of `samples`.
/// 2. Compute the **MAD** (Median Absolute Deviation from the median).
/// 3. Determine `best_improvement`:
///    - `Lower`: `baseline − min(samples)` (positive ↔ improvement).
///    - `Higher`: `max(samples) − baseline` (positive ↔ improvement).
/// 4. `multiplier = best_improvement / MAD` (clamped to `0.0` when MAD ≈ 0
///    *and* there is no improvement, or set to `f64::INFINITY` when MAD ≈ 0
///    but there *is* an improvement).
pub fn compute(samples: &[f64], baseline: f64, direction: MetricDirection) -> ConfidenceScore {
    if samples.len() < 3 {
        return ConfidenceScore {
            multiplier: 0.0,
            band: ConfidenceBand::Insufficient,
        };
    }

    // ── median ────────────────────────────────────────────────────────────────
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = median_of_sorted(&sorted);

    // ── MAD ───────────────────────────────────────────────────────────────────
    let mut deviations: Vec<f64> = sorted.iter().map(|x| (x - median).abs()).collect();
    deviations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mad = median_of_sorted(&deviations);

    // ── best improvement ──────────────────────────────────────────────────────
    let best_improvement = match direction {
        MetricDirection::Lower => {
            let min = sorted.first().copied().unwrap_or(baseline);
            baseline - min
        }
        MetricDirection::Higher => {
            let max = sorted.last().copied().unwrap_or(baseline);
            max - baseline
        }
    };

    // ── multiplier ────────────────────────────────────────────────────────────
    let multiplier = if mad.abs() < f64::EPSILON {
        // Zero variability: if there is any improvement call it "infinite",
        // otherwise zero.
        if best_improvement > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    } else {
        (best_improvement / mad).max(0.0)
    };

    // ── band ──────────────────────────────────────────────────────────────────
    let band = if multiplier >= 2.0 {
        ConfidenceBand::Green
    } else if multiplier >= 1.0 {
        ConfidenceBand::Yellow
    } else {
        ConfidenceBand::Red
    };

    ConfidenceScore { multiplier, band }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Median of a **pre-sorted** slice (panics on empty input).
fn median_of_sorted(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 0 {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}
