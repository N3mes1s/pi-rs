//! Dashboard widget — pure rendering helpers, no I/O.
//!
//! [`render_inline`] returns a single-line widget string:
//! ```text
//! 🔬 autoresearch 12 runs 8 kept │ ★ total_µs: 15,200 (-12.3%) │ conf: 2.1×
//! ```
//!
//! [`render_table`] returns a multi-line table of all runs for the expanded
//! view.

use crate::autoresearch::confidence::{ConfidenceBand, ConfidenceScore};
use crate::autoresearch::session::MetricDirection;

// ── DashboardState ────────────────────────────────────────────────────────────

/// Snapshot of an autoresearch session used to drive both renderers.
pub struct DashboardState {
    /// Human-readable experiment name.
    pub session_name: String,
    /// Total number of runs attempted.
    pub runs: usize,
    /// Number of runs whose result was `kept = true`.
    pub kept: usize,
    /// Name of the metric being optimised.
    pub metric_name: String,
    /// Pre-experiment reference value.
    pub baseline: f64,
    /// Best observed metric value so far.
    pub current_best: f64,
    /// Whether lower or higher values are improvements.
    pub direction: MetricDirection,
    /// Confidence score computed from the run samples.
    pub confidence: ConfidenceScore,
}

// ── render_inline ─────────────────────────────────────────────────────────────

/// Return a single-line dashboard widget.
///
/// Format:
/// `🔬 autoresearch <runs> runs <kept> kept │ ★ <metric>: <best> (<pct>%) │ conf: <mult>×`
pub fn render_inline(state: &DashboardState) -> String {
    let pct = percent_change(state.baseline, state.current_best, state.direction);
    let pct_str = format_pct(pct);
    let conf_str = format_conf(&state.confidence);
    let best_str = format_value(state.current_best);

    format!(
        "🔬 autoresearch {} runs {} kept │ ★ {}: {} ({}) │ conf: {}",
        state.runs,
        state.kept,
        state.metric_name,
        best_str,
        pct_str,
        conf_str,
    )
}

// ── render_table ──────────────────────────────────────────────────────────────

/// Return a multi-line table for the expanded view.
///
/// `runs` is a slice of `(idea, metric_value, kept)` tuples in the order
/// they were recorded.  The table includes a header, a separator, and one
/// row per run, followed by a summary footer.
pub fn render_table(state: &DashboardState, runs: &[(String, f64, bool)]) -> String {
    let mut out = String::new();

    // ── header ────────────────────────────────────────────────────────────────
    out.push_str(&format!(
        "autoresearch: {}\n",
        state.session_name
    ));
    out.push_str(&format!(
        "metric: {} | baseline: {} | direction: {}\n",
        state.metric_name,
        format_value(state.baseline),
        match state.direction {
            MetricDirection::Lower => "lower",
            MetricDirection::Higher => "higher",
        },
    ));
    out.push_str(&format!(
        "runs: {} | kept: {} | best: {} | conf: {}\n",
        state.runs,
        state.kept,
        format_value(state.current_best),
        format_conf(&state.confidence),
    ));

    // ── column header ─────────────────────────────────────────────────────────
    out.push_str(&format!(
        "\n{:<4} {:<40} {:>12} {}\n",
        "#", "idea", state.metric_name, "kept"
    ));
    out.push_str(&format!("{}\n", "-".repeat(62)));

    // ── rows ──────────────────────────────────────────────────────────────────
    for (i, (idea, value, kept)) in runs.iter().enumerate() {
        let truncated = if idea.len() > 40 {
            format!("{}…", &idea[..39])
        } else {
            idea.clone()
        };
        out.push_str(&format!(
            "{:<4} {:<40} {:>12} {}\n",
            i + 1,
            truncated,
            format_value(*value),
            if *kept { "✓" } else { "✗" },
        ));
    }

    // ── footer ────────────────────────────────────────────────────────────────
    if !runs.is_empty() {
        let pct = percent_change(state.baseline, state.current_best, state.direction);
        out.push_str(&format!("\nbest improvement: {}\n", format_pct(pct)));
    }

    out
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Percent change relative to `baseline`, signed so that an *improvement* is
/// always negative for Lower and positive for Higher.
///
/// Returns `0.0` when `baseline` is zero (avoids division by zero).
fn percent_change(baseline: f64, best: f64, direction: MetricDirection) -> f64 {
    if baseline == 0.0 {
        return 0.0;
    }
    match direction {
        MetricDirection::Lower => (best - baseline) / baseline.abs() * 100.0,
        MetricDirection::Higher => (best - baseline) / baseline.abs() * 100.0,
    }
}

/// Format a percentage with a sign and one decimal place.
fn format_pct(pct: f64) -> String {
    if pct >= 0.0 {
        format!("+{:.1}%", pct)
    } else {
        format!("{:.1}%", pct)
    }
}

/// Format the confidence score as `<multiplier>× (<band>)`.
fn format_conf(score: &ConfidenceScore) -> String {
    let band_char = match score.band {
        ConfidenceBand::Green => "🟢",
        ConfidenceBand::Yellow => "🟡",
        ConfidenceBand::Red => "🔴",
        ConfidenceBand::Insufficient => "⚪",
    };
    if score.multiplier.is_infinite() {
        format!("∞× {}", band_char)
    } else {
        format!("{:.1}× {}", score.multiplier, band_char)
    }
}

/// Format a metric value with thousands-separators and up to one decimal place.
fn format_value(v: f64) -> String {
    // Use comma grouping for the integer part.
    let rounded = v.round() as i64;
    let s = rounded.to_string();
    let is_neg = s.starts_with('-');
    let digits: &str = if is_neg { &s[1..] } else { &s };

    let mut grouped = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let grouped: String = grouped.chars().rev().collect();
    if is_neg {
        format!("-{}", grouped)
    } else {
        grouped
    }
}
