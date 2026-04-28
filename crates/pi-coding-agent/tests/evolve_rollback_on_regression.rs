//! RFD 0013 — rollback watchdog reverts AGENTS.md on regression.

use pi_coding_agent::evolve::apply::{append_history, HistoryEntry};
use pi_coding_agent::evolve::rollback::{tick, RollbackOutcome};

fn seed_apply(history: &std::path::Path, prev_body: &str) {
    let entry = HistoryEntry {
        ts: "2026-04-28T00:00:00Z".to_string(),
        action: "apply".to_string(),
        from_hash: "ab12".to_string(),
        to_hash: "cd34".to_string(),
        pre_mean: Some(0.80),
        post_mean_estimate: Some(0.92),
        margin: Some(0.12),
        observed_mean: None,
        trigger: None,
        prev_body: Some(prev_body.to_string()),
    };
    append_history(history, &entry).unwrap();
}

#[test]
fn rollback_reverts_when_observed_mean_drops_below_pre_minus_margin() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    let history = tmp.path().join("history.jsonl");

    std::fs::write(&agents, "CANDIDATE BODY\n").unwrap();
    seed_apply(&history, "ORIGINAL BODY\n");

    // 5 outcome means well below 0.80 - 0.10 = 0.70.
    let lows = [0.4, 0.5, 0.45, 0.55, 0.50];
    let out = tick(&agents, &history, &lows, 5, 0.10).unwrap();
    match out {
        RollbackOutcome::RolledBack { observed_mean } => {
            assert!(observed_mean < 0.7);
        }
        other => panic!("expected RolledBack, got {other:?}"),
    }

    assert_eq!(
        std::fs::read_to_string(&agents).unwrap(),
        "ORIGINAL BODY\n",
        "AGENTS.md must be restored to the pre-apply body"
    );

    let rows: Vec<_> = std::fs::read_to_string(&history)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<HistoryEntry>(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].action, "rollback");
    assert_eq!(rows[1].from_hash, "cd34");
    assert_eq!(rows[1].to_hash, "ab12");
    assert!(rows[1].observed_mean.unwrap() < 0.7);
}

#[test]
fn rollback_holds_when_mean_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    let history = tmp.path().join("history.jsonl");
    std::fs::write(&agents, "CANDIDATE\n").unwrap();
    seed_apply(&history, "OLD\n");

    let highs = [0.85, 0.9, 0.82, 0.78, 0.81];
    let out = tick(&agents, &history, &highs, 5, 0.10).unwrap();
    matches!(out, RollbackOutcome::Held { .. });
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "CANDIDATE\n");
}

#[test]
fn rollback_waits_for_min_samples() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    let history = tmp.path().join("history.jsonl");
    std::fs::write(&agents, "CANDIDATE\n").unwrap();
    seed_apply(&history, "OLD\n");

    let two = [0.1, 0.1];
    let out = tick(&agents, &history, &two, 5, 0.10).unwrap();
    assert!(matches!(out, RollbackOutcome::InsufficientSamples { .. }));
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "CANDIDATE\n");
}

#[test]
fn rollback_no_pending_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("AGENTS.md");
    let history = tmp.path().join("history.jsonl");
    let out = tick(&agents, &history, &[0.1, 0.1, 0.1, 0.1, 0.1], 5, 0.10).unwrap();
    assert_eq!(out, RollbackOutcome::NoPendingApply);
}
