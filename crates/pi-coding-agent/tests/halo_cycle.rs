// Canonical M2 tests — names only, implementation stubs.

#[test]
fn mocked_orchestrate_two_cycle_test() {
    // TODO: implement test harness
}

#[test]
fn evolve_tick_applies_then_next_cycle_clean_tree() {
}

#[test]
fn local_target_branch_authoritative() {
}

/// A dirty working tree at cycle start must abort the cycle before any
/// step that could mutate the repo. Regression for a live-observed
/// failure: an uncommitted edit left by a killed implementer survived
/// into the next cycle and poisoned its post-orchestrate checkout /
/// `git revert`, pausing the supervisor. The gate (`step_tree_clean_check`,
/// wired at cycle start) refuses immediately instead.
///
/// Runs entirely inside a throwaway git repo — never the live checkout.
#[test]
fn dirty_tree_at_startup_refused() {
    use pi_coding_agent::halo::cycle::{
        build_ctx, default_config, run_cycle_with_ctx, CycleOutcome,
    };
    use std::sync::atomic::{AtomicBool, AtomicI32};
    use std::sync::Arc;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-C", &repo.display().to_string()])
            .args(args)
            .output()
            .expect("spawn git")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path();
    let halo_dir = repo.join(".halo");
    std::fs::create_dir_all(&halo_dir).unwrap();

    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "one\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "init"]);

    // Dirty the tree — the leftover a killed implementer would leave.
    std::fs::write(repo.join("f.txt"), "uncommitted change\n").unwrap();

    let cfg = default_config();
    let ctx = build_ctx(
        repo,
        &halo_dir,
        1,
        &cfg,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicI32::new(0)),
    );

    let outcome = run_cycle_with_ctx(repo, 1, ctx).expect("cycle runs");
    match outcome {
        CycleOutcome::Aborted { reason } => {
            assert_eq!(reason, "dirty working tree", "unexpected abort reason");
        }
        other => panic!("expected Aborted on dirty tree, got {other:?}"),
    }

    // The refusal must be recorded, and the dirty file left untouched
    // (the gate refuses, it does not silently discard operator work).
    let state = std::fs::read_to_string(halo_dir.join("state.jsonl")).unwrap_or_default();
    assert!(
        state.contains("STEP_TREE_DIRTY_REFUSED"),
        "expected STEP_TREE_DIRTY_REFUSED in state.jsonl, got: {state}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("f.txt")).unwrap(),
        "uncommitted change\n",
        "gate must not touch the dirty file"
    );
}

#[test]
fn missing_repo_local_agents_md_refused() {
}

#[test]
fn no_detached_evolve_during_orchestrate() {
}

#[test]
fn keep_marker_violation_routes_through_rollback_before_smoke() {
}

#[test]
fn post_orchestrate_checkout_returns_to_target_branch_on_failed_exit() {
}

#[test]
fn foreground_sigint_graceful_drain_paused_and_exited() {
}

#[test]
fn kill_minus_9_mid_cycle_recovery_emits_exactly_one_supervisor_crashed() {
}

#[test]
fn proposer_failure_3x_emits_step_proposer_failed_outcome_failed() {
}

#[test]
fn orchestrate_exit_3_blocked_outcome_halo_continues() {
}
