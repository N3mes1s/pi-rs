//! Lock the v0 stub-runner contract. v1 (real spawn) will swap the
//! body of `dispatch_stub` but must keep these guarantees:
//!
//!   * each milestone produces exactly two events: PENDING→DISPATCHED
//!     and DISPATCHED→<terminal-state>
//!   * the order of events follows topological order of the campaign
//!   * `state.jsonl` is appended (multiple runs accumulate, not
//!     overwrite) so resume is possible
//!   * truncated trailing lines in state.jsonl don't crash `replay`
//!     (RFD 0021 §"Persisted state layout": partial writes can never
//!     corrupt state)
//!   * campaign names with `/` or `\` don't escape the state root
//!
//! v0 fills the terminal slot with `STUBBED_COMPLETE`; v1 will use
//! `MERGED` / `FAILED` / etc. The structural invariants above hold
//! regardless.

use pi_orchestrate::{parse_campaign, replay, run, state_path_for, validate, StateEvent};
use std::fs::OpenOptions;
use std::io::Write;
use tempfile::tempdir;

const SIMPLE_CAMPAIGN_TOML: &str = r#"
name = "simple-test"
target_branch = "main"

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"

[[milestones]]
id = "beta"
branch = "feat/beta"
depends_on = ["alpha"]
implementer = "router-implementer"
assignment = "do beta after alpha"

[[milestones]]
id = "gamma"
branch = "feat/gamma"
depends_on = ["alpha"]
implementer = "router-implementer"
assignment = "do gamma after alpha (parallel with beta)"
"#;

fn parse_and_validate(toml: &str) -> pi_orchestrate::Campaign {
    let campaign = parse_campaign(toml).expect("parses");
    if let Err(errs) = validate(&campaign) {
        panic!("validate: {errs:?}");
    }
    campaign
}

#[test]
fn run_emits_two_events_per_milestone_in_topo_order() {
    let campaign = parse_and_validate(SIMPLE_CAMPAIGN_TOML);
    let state_root = tempdir().unwrap();
    let summary = run(&campaign, state_root.path()).expect("run ok");

    assert_eq!(summary.outcomes.len(), 3);
    assert_eq!(summary.outcomes[0].id, "alpha");
    assert!(
        summary.outcomes[1].id == "beta" || summary.outcomes[1].id == "gamma",
        "second milestone must be a peer of alpha's children"
    );

    let events = replay(&summary.state_path).unwrap();
    assert_eq!(
        events.len(),
        6,
        "3 milestones × 2 transitions each must be 6 events"
    );
    // Every event for alpha precedes any event for beta or gamma.
    let alpha_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.milestone == "alpha")
        .map(|(i, _)| i)
        .collect();
    let other_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.milestone != "alpha")
        .map(|(i, _)| i)
        .collect();
    let last_alpha = *alpha_indices.last().unwrap();
    let first_other = *other_indices.first().unwrap();
    assert!(
        last_alpha < first_other,
        "all alpha events must come before any beta/gamma event (topo order); got alpha={alpha_indices:?} other={other_indices:?}"
    );
}

#[test]
fn each_milestone_has_pending_to_dispatched_to_terminal() {
    let campaign = parse_and_validate(SIMPLE_CAMPAIGN_TOML);
    let state_root = tempdir().unwrap();
    let summary = run(&campaign, state_root.path()).unwrap();
    let events = replay(&summary.state_path).unwrap();

    for milestone in ["alpha", "beta", "gamma"] {
        let mine: Vec<&StateEvent> =
            events.iter().filter(|e| e.milestone == milestone).collect();
        assert_eq!(
            mine.len(),
            2,
            "milestone {milestone} must have exactly 2 events"
        );
        assert_eq!(mine[0].from, "PENDING");
        assert_eq!(mine[0].to, "DISPATCHED");
        assert_eq!(mine[1].from, "DISPATCHED");
        assert_eq!(
            mine[1].to, "STUBBED_COMPLETE",
            "v0 terminal state is STUBBED_COMPLETE"
        );
    }
}

#[test]
fn second_run_appends_does_not_overwrite() {
    // RFD §"Persisted state layout": resume is built by replay; runs
    // append. Two consecutive runs must produce 12 events on disk
    // (3 milestones × 2 events × 2 runs).
    let campaign = parse_and_validate(SIMPLE_CAMPAIGN_TOML);
    let state_root = tempdir().unwrap();
    let _ = run(&campaign, state_root.path()).unwrap();
    let summary2 = run(&campaign, state_root.path()).unwrap();
    let events = replay(&summary2.state_path).unwrap();
    assert_eq!(events.len(), 12);
}

#[test]
fn replay_drops_truncated_trailing_line() {
    let campaign = parse_and_validate(SIMPLE_CAMPAIGN_TOML);
    let state_root = tempdir().unwrap();
    let summary = run(&campaign, state_root.path()).unwrap();
    // Append a half-written line — simulates a crash mid-write.
    let mut f = OpenOptions::new()
        .append(true)
        .open(&summary.state_path)
        .unwrap();
    f.write_all(b"{\"milestone\":\"alpha\",\"from\":\"X\"")
        .unwrap();
    drop(f);
    let events = replay(&summary.state_path).unwrap();
    // The 6 valid events must still be there; the truncated 7th is
    // silently dropped.
    assert_eq!(events.len(), 6);
}

#[test]
fn campaign_name_with_slashes_is_sanitised() {
    // Defence in depth: a TOML name field with `/` must not let the
    // state file escape the configured root.
    let toml = r#"
name = "../../../etc/passwd"
target_branch = "main"

[[milestones]]
id = "x"
branch = "feat/x"
implementer = "router-implementer"
assignment = "test"
"#;
    let campaign = parse_campaign(toml).unwrap();
    // The validator may or may not flag this — we don't depend on it.
    // Just confirm `state_path_for` produces a path UNDER the root.
    let state_root = tempdir().unwrap();
    let p = state_path_for(state_root.path(), &campaign.name).unwrap();
    let canonical_root = state_root.path().canonicalize().unwrap();
    let canonical_p = p
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap_or_else(|_| p.parent().unwrap().to_path_buf());
    assert!(
        canonical_p.starts_with(&canonical_root),
        "sanitised path must stay under root: root={}, parent={}",
        canonical_root.display(),
        canonical_p.display()
    );
}

#[test]
fn empty_campaign_runs_clean() {
    let toml = r#"
name = "empty-test"
target_branch = "main"
"#;
    let campaign = parse_campaign(toml).unwrap();
    let state_root = tempdir().unwrap();
    let summary = run(&campaign, state_root.path()).unwrap();
    assert!(summary.outcomes.is_empty());
    let events = replay(&summary.state_path).unwrap();
    assert!(events.is_empty());
}
