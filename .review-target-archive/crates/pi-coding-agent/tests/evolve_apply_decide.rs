//! RFD 0013 — `evolve::apply::decide` margin-gate unit tests.

use pi_coding_agent::evolve::apply::decide;

#[test]
fn margin_met_applies() {
    let d = decide(&[0.5, 0.6, 0.7], &[0.7, 0.8, 0.9], 0.10);
    assert!(d.apply, "candidate beats current by 0.20 ≥ 0.10: {d:?}");
    assert!((d.candidate_mean - 0.8).abs() < 1e-5);
    assert!((d.current_mean - 0.6).abs() < 1e-5);
    assert!((d.margin - 0.2).abs() < 1e-5);
}

#[test]
fn margin_not_met_declines() {
    let d = decide(&[0.6, 0.6, 0.6], &[0.62, 0.65, 0.63], 0.10);
    assert!(!d.apply);
    assert!(d.reason.contains("declined"));
}

#[test]
fn equal_means_decline_when_margin_positive() {
    let d = decide(&[0.5], &[0.5], 0.10);
    assert!(!d.apply, "zero gain must not exceed positive margin");
}

#[test]
fn zero_margin_accepts_tie() {
    let d = decide(&[0.5, 0.7], &[0.5, 0.7], 0.0);
    assert!(d.apply, "tie meets a 0.0 margin");
}

#[test]
fn nan_in_inputs_skipped_in_mean() {
    let d = decide(&[0.5, f32::NAN, 0.9], &[0.6, f32::NAN, 1.0], 0.05);
    // Means: cur=0.7, cand=0.8. Margin=0.1 ≥ 0.05.
    assert!(d.apply, "{d:?}");
    assert!((d.current_mean - 0.7).abs() < 1e-5);
    assert!((d.candidate_mean - 0.8).abs() < 1e-5);
}

#[test]
fn empty_current_declines() {
    let d = decide(&[], &[0.7, 0.8], 0.10);
    assert!(!d.apply);
    assert!(d.current_mean.is_nan());
}

#[test]
fn empty_candidate_declines() {
    let d = decide(&[0.7], &[], 0.10);
    assert!(!d.apply);
    assert!(d.candidate_mean.is_nan());
}

#[test]
fn single_value_distributions() {
    let d = decide(&[0.4], &[0.7], 0.10);
    assert!(d.apply);
    let d2 = decide(&[0.4], &[0.45], 0.10);
    assert!(!d2.apply);
}

#[test]
fn many_value_distributions() {
    let cur: Vec<f32> = (0..50).map(|i| 0.5 + (i as f32) * 0.001).collect();
    let cand: Vec<f32> = (0..50).map(|i| 0.7 + (i as f32) * 0.001).collect();
    let d = decide(&cur, &cand, 0.10);
    assert!(d.apply, "0.20 lift over 50 samples must clear 0.10 margin");
}
