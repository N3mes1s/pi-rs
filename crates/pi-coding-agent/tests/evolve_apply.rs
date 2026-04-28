//! Tests for Pareto frontier + apply / rollback (G9).

use pi_coding_agent::evolve::{
    add_poison, append_generation, backup_and_apply, best_strict_improvement, is_poisoned,
    pareto_frontier, poisoned_hashes, read_generations, should_rollback,
    BenchmarkSummary, Candidate, GenerationLogEntry, PendingApply,
};
// Disambiguate from the new `evolve::rollback` module (RFD 0013).
use pi_coding_agent::evolve::apply::rollback;

fn summary(pass_rate: f32, mean_score: f32, p95: u64, cost: f32) -> BenchmarkSummary {
    BenchmarkSummary {
        n_cases: 10,
        pass_rate,
        mean_score,
        mean_tokens_in: 1000.0,
        mean_tokens_out: 200.0,
        p95_tokens_in: p95,
        total_cost_usd: cost,
        mean_duration_ms: 500.0,
    }
}

fn cand(hash: &str, summary: BenchmarkSummary) -> Candidate {
    Candidate {
        hash: hash.into(),
        summary,
        body: format!("body for {hash}"),
        mutated_section: Some(0),
        note: "test".into(),
    }
}

// ─── Pareto frontier ───────────────────────────────────────────────────

#[test]
fn pareto_frontier_singleton() {
    let cands = vec![cand("a", summary(0.5, 0.5, 1000, 0.01))];
    let f = pareto_frontier(&cands);
    assert_eq!(f, vec![0]);
}

#[test]
fn pareto_frontier_excludes_dominated() {
    // b strictly dominates a on every axis.
    let cands = vec![
        cand("a", summary(0.5, 0.5, 2000, 0.02)),
        cand("b", summary(0.7, 0.7, 1000, 0.01)),
    ];
    let f = pareto_frontier(&cands);
    assert_eq!(f, vec![1]);
}

#[test]
fn pareto_frontier_keeps_incomparable_pair() {
    // a: high pass rate, expensive. b: lower pass rate, cheaper.
    let cands = vec![
        cand("a", summary(0.9, 0.9, 2000, 0.05)),
        cand("b", summary(0.7, 0.7, 1000, 0.01)),
    ];
    let f = pareto_frontier(&cands);
    assert_eq!(f.len(), 2, "neither dominates the other");
}

#[test]
fn pareto_frontier_ties_count_as_non_dominated() {
    // Identical summaries: neither dominates the other (no strict-better axis).
    let cands = vec![
        cand("a", summary(0.7, 0.7, 1000, 0.01)),
        cand("b", summary(0.7, 0.7, 1000, 0.01)),
    ];
    let f = pareto_frontier(&cands);
    assert_eq!(f.len(), 2);
}

// ─── best_strict_improvement ──────────────────────────────────────────

#[test]
fn best_strict_improvement_picks_best_pareto_winner() {
    let cands = vec![
        cand("baseline", summary(0.6, 0.6, 1500, 0.02)),
        cand("better-a", summary(0.7, 0.7, 1400, 0.018)),
        cand("better-b", summary(0.8, 0.75, 1200, 0.015)),
    ];
    let best = best_strict_improvement(&cands, 0).expect("a winner");
    // better-b dominates baseline AND better-a; should be the pick.
    assert_eq!(cands[best].hash, "better-b");
}

#[test]
fn best_strict_improvement_refuses_pass_rate_regression() {
    // Candidate is cheaper but pass rate dropped — disallow.
    let cands = vec![
        cand("baseline", summary(0.7, 0.7, 1500, 0.02)),
        cand("cheap-but-worse", summary(0.5, 0.6, 1000, 0.01)),
    ];
    let best = best_strict_improvement(&cands, 0);
    assert!(best.is_none(), "pass-rate regression must not win");
}

#[test]
fn best_strict_improvement_returns_none_on_no_improvers() {
    let cands = vec![cand("only", summary(0.7, 0.7, 1000, 0.01))];
    assert!(best_strict_improvement(&cands, 0).is_none());
}

#[test]
fn best_strict_improvement_accepts_token_only_win() {
    // Same pass + score but cheaper p95 tokens — clear win.
    let cands = vec![
        cand("baseline", summary(0.7, 0.7, 2000, 0.02)),
        cand("leaner", summary(0.7, 0.7, 1000, 0.02)),
    ];
    let best = best_strict_improvement(&cands, 0).expect("a winner");
    assert_eq!(cands[best].hash, "leaner");
}

// ─── backup_and_apply / rollback ──────────────────────────────────────

#[test]
fn apply_writes_new_body_and_backs_up_old() {
    let dir = tempfile::tempdir().unwrap();
    let agents_md = dir.path().join("AGENTS.md");
    std::fs::write(&agents_md, "old content").unwrap();

    let backup = backup_and_apply(dir.path(), &agents_md, "new content", "abc123def456").unwrap();
    assert_eq!(std::fs::read_to_string(&agents_md).unwrap(), "new content");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "old content");
    assert!(backup.to_string_lossy().contains("abc123def456"));
}

#[test]
fn rollback_restores_old_body() {
    let dir = tempfile::tempdir().unwrap();
    let agents_md = dir.path().join("AGENTS.md");
    std::fs::write(&agents_md, "v1").unwrap();
    let backup = backup_and_apply(dir.path(), &agents_md, "v2", "h1").unwrap();
    rollback(&agents_md, &backup).unwrap();
    assert_eq!(std::fs::read_to_string(&agents_md).unwrap(), "v1");
}

#[test]
fn apply_when_no_prior_file_creates_empty_backup() {
    let dir = tempfile::tempdir().unwrap();
    let agents_md = dir.path().join("AGENTS.md");
    let backup = backup_and_apply(dir.path(), &agents_md, "v1", "h0").unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), b"" as &[u8]);
    assert_eq!(std::fs::read_to_string(&agents_md).unwrap(), "v1");

    // Rollback removes the file (since backup is empty marker).
    rollback(&agents_md, &backup).unwrap();
    assert!(!agents_md.exists());
}

// ─── PendingApply ─────────────────────────────────────────────────────

#[test]
fn pending_apply_persists_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let p = PendingApply {
        applied_hash: "newhash".into(),
        previous_hash: "oldhash".into(),
        backup_path: dir.path().join("backup.md"),
        baseline_pass_rate: 0.7,
        applied_at_ms: 1700000000000,
        outcomes_seen_at_apply: 50,
    };
    p.save(dir.path()).unwrap();
    let back = PendingApply::load(dir.path()).expect("loaded");
    assert_eq!(back.applied_hash, "newhash");
    assert_eq!(back.baseline_pass_rate, 0.7);

    PendingApply::clear(dir.path()).unwrap();
    assert!(PendingApply::load(dir.path()).is_none());
}

// ─── should_rollback ──────────────────────────────────────────────────

#[test]
fn rollback_skipped_until_window_filled() {
    // 3 sessions seen; need 10. Don't roll back even on bad pass rate.
    assert!(!should_rollback(0.8, 0.2, 3, 10, 0.15));
}

#[test]
fn rollback_triggers_on_significant_regression() {
    // Window full + pass rate dropped 30% → rollback.
    assert!(should_rollback(0.8, 0.5, 10, 10, 0.15));
}

#[test]
fn rollback_skipped_on_minor_drop() {
    // Drop of 0.05 < threshold 0.15.
    assert!(!should_rollback(0.8, 0.75, 10, 10, 0.15));
}

#[test]
fn rollback_skipped_when_pass_rate_improves() {
    // New > baseline → never roll back.
    assert!(!should_rollback(0.7, 0.85, 20, 10, 0.15));
}

// ─── Poison list ─────────────────────────────────────────────────────

#[test]
fn poison_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_poisoned(dir.path(), "abc"));
    add_poison(dir.path(), "abc").unwrap();
    add_poison(dir.path(), "def").unwrap();
    assert!(is_poisoned(dir.path(), "abc"));
    assert!(is_poisoned(dir.path(), "def"));
    assert!(!is_poisoned(dir.path(), "ghi"));
    let all = poisoned_hashes(dir.path());
    assert_eq!(all.len(), 2);
}

// ─── Generations log ─────────────────────────────────────────────────

#[test]
fn generations_log_appends_and_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let entry = GenerationLogEntry {
        timestamp_ms: 1700000000000,
        hash: "h1".into(),
        parent_hash: Some("h0".into()),
        mutated_section: Some(2),
        summary: summary(0.7, 0.7, 1000, 0.01),
        applied: false,
        note: "first gen".into(),
    };
    append_generation(dir.path(), &entry).unwrap();

    let entry2 = GenerationLogEntry {
        timestamp_ms: 1700000001000,
        hash: "h2".into(),
        parent_hash: Some("h1".into()),
        mutated_section: Some(0),
        summary: summary(0.8, 0.75, 900, 0.012),
        applied: true,
        note: "applied as new baseline".into(),
    };
    append_generation(dir.path(), &entry2).unwrap();

    let entries = read_generations(dir.path());
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].hash, "h1");
    assert_eq!(entries[1].hash, "h2");
    assert!(entries[1].applied);
}
