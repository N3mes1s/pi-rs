//! v1 runner integration tests.
//!
//! Each test sets up a tempdir git repo with two milestone branches
//! and a target branch, then runs the orchestrator against a
//! `FakeDispatch` that returns canned verdicts. We assert state.jsonl
//! shape, exit code, and final-state outcomes.

use pi_orchestrate::dispatch::{Dispatch, DispatchOutcome, DispatchRole};
use pi_orchestrate::{parse_campaign, replay, run_with, validate};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use tempfile::tempdir;

/// FakeDispatch — returns canned outcomes per call. Lets a test feed
/// the runner a script of "implementer-1, reviewer-1, implementer-2,
/// reviewer-2, ..." in dispatch order without spawning subprocesses.
struct FakeDispatch {
    canned: Mutex<Vec<DispatchOutcome>>,
    calls: Mutex<Vec<(DispatchRole, String)>>,
}

impl FakeDispatch {
    fn new(canned: Vec<DispatchOutcome>) -> Self {
        Self {
            canned: Mutex::new(canned),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<(DispatchRole, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Dispatch for FakeDispatch {
    fn dispatch(
        &self,
        role: DispatchRole,
        agent_name: &str,
        _assignment: &str,
        _cwd: &Path,
    ) -> std::io::Result<DispatchOutcome> {
        self.calls
            .lock()
            .unwrap()
            .push((role, agent_name.to_string()));
        let mut q = self.canned.lock().unwrap();
        if q.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "FakeDispatch ran out of canned outcomes",
            ));
        }
        Ok(q.remove(0))
    }
}

fn ok(text: &str) -> DispatchOutcome {
    DispatchOutcome {
        agent: "fake".into(),
        success: true,
        model_output: text.to_string(),
        stderr: String::new(),
        exit_code: 0,
        duration_ms: 0,
    }
}

fn fail(stderr: &str) -> DispatchOutcome {
    DispatchOutcome {
        agent: "fake".into(),
        success: false,
        model_output: String::new(),
        stderr: stderr.to_string(),
        exit_code: 1,
        duration_ms: 0,
    }
}

/// Set up a tempdir git repo with `target_branch` and one milestone
/// branch each adding a unique file. Returns the repo root path.
fn make_repo(target: &str, milestone_branches: &[&str]) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let p = dir.path();
    fn run(p: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(p).output().unwrap();
        if !out.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    run(p, &["init", "-q", "-b", target]);
    run(p, &["config", "user.email", "test@example.com"]);
    run(p, &["config", "user.name", "Test"]);
    // Disable gpg signing — the user's global config has it on, but a
    // throwaway test repo doesn't have access to the user's key (and
    // shouldn't anyway).
    run(p, &["config", "commit.gpgsign", "false"]);
    run(p, &["config", "tag.gpgsign", "false"]);
    std::fs::write(p.join("README.md"), "base\n").unwrap();
    run(p, &["add", "README.md"]);
    run(p, &["commit", "-q", "-m", "base"]);
    for (i, b) in milestone_branches.iter().enumerate() {
        run(p, &["checkout", "-q", "-b", b]);
        let fname = format!("feat-{i}.txt");
        std::fs::write(p.join(&fname), format!("milestone {i}\n")).unwrap();
        run(p, &["add", &fname]);
        run(p, &["commit", "-q", "-m", &format!("feat {i}")]);
        run(p, &["checkout", "-q", target]);
    }
    dir
}

const TWO_MILESTONE_TOML: &str = r#"
name = "two-ms-test"
target_branch = "main"

[defaults]
fix_loop_max = 2

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
assignment = "do beta"
"#;

fn parse(toml: &str) -> pi_orchestrate::Campaign {
    let c = parse_campaign(toml).unwrap();
    validate(&c).unwrap();
    c
}

// ─── happy path: both milestones merge cleanly ───────────────────

#[test]
fn ready_to_merge_both_milestones_reach_merged_exit_0() {
    let repo = make_repo("main", &["feat/alpha", "feat/beta"]);
    let dispatcher = FakeDispatch::new(vec![
        ok("implementer alpha output"),
        ok("looks great\n\nMerge readiness: READY_TO_MERGE"),
        ok("implementer beta output"),
        ok("ditto\n\nMerge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary = run_with(&parse(TWO_MILESTONE_TOML), state_root.path(), &dispatcher, repo.path())
        .unwrap();

    assert_eq!(summary.exit_code, 0);
    assert_eq!(summary.outcomes.len(), 2);
    for o in &summary.outcomes {
        assert_eq!(o.final_state, "MERGED", "milestone {} should be MERGED", o.id);
    }

    // State.jsonl should have for each milestone:
    //   PENDING → DISPATCHED
    //   DISPATCHED → REVIEWED
    //   REVIEWED → MERGE_PENDING
    //   MERGE_PENDING → MERGED
    // = 4 events × 2 milestones = 8 events.
    let events = replay(&summary.state_path).unwrap();
    assert_eq!(events.len(), 8, "expected 8 events, got {events:#?}");

    // Both branches are now in the target branch's history.
    let target_log = Command::new("git")
        .args(["log", "--oneline", "main"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&target_log.stdout);
    assert!(log.contains("feat 0"), "alpha must be on main: {log}");
    assert!(log.contains("feat 1"), "beta must be on main: {log}");
}

// ─── fix-loop: NEEDS_FIX once then READY ─────────────────────────

#[test]
fn fix_loop_one_iteration_then_ready() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "fix-loop-test"
target_branch = "main"

[defaults]
fix_loop_max = 2

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    let dispatcher = FakeDispatch::new(vec![
        ok("first attempt"),
        ok("hmm\n\nMerge readiness: NEEDS_FIX"),
        ok("second attempt"),
        ok("better\n\nMerge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    assert_eq!(summary.exit_code, 0);
    assert_eq!(summary.outcomes[0].final_state, "MERGED");
    assert_eq!(summary.outcomes[0].fix_loop_iterations, 2);

    // 4 calls: implementer, reviewer, implementer, reviewer.
    assert_eq!(dispatcher.calls().len(), 4);
}

// ─── fix-loop exhausted → FAILED, exit 2 ─────────────────────────

#[test]
fn fix_loop_exhausted_marks_failed_exit_2() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "exhausted-test"
target_branch = "main"

[defaults]
fix_loop_max = 2

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    // Reviewer always says NEEDS_FIX. Two iterations → exhaust → FAILED.
    let dispatcher = FakeDispatch::new(vec![
        ok("attempt 1"),
        ok("Merge readiness: NEEDS_FIX"),
        ok("attempt 2"),
        ok("Merge readiness: NEEDS_FIX"),
    ]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    assert_eq!(summary.exit_code, 2);
    assert_eq!(summary.outcomes[0].final_state, "FAILED");
    assert_eq!(summary.outcomes[0].fix_loop_iterations, 2);
}

// ─── DO_NOT_MERGE → immediate FAILED, no further iterations ──────

#[test]
fn do_not_merge_fails_immediately() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "dnm-test"
target_branch = "main"

[defaults]
fix_loop_max = 5

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    let dispatcher = FakeDispatch::new(vec![
        ok("attempt 1"),
        ok("Merge readiness: DO_NOT_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    assert_eq!(summary.exit_code, 2);
    assert_eq!(summary.outcomes[0].final_state, "FAILED");
    // Must have used exactly ONE iteration even though fix_loop_max = 5.
    assert_eq!(summary.outcomes[0].fix_loop_iterations, 1);
}

// ─── failed dependency cascades ──────────────────────────────────

#[test]
fn failed_dependency_blocks_descendant() {
    let repo = make_repo("main", &["feat/alpha", "feat/beta"]);
    // alpha fails → beta must be skipped (FAILED with blocked-by reason).
    let dispatcher = FakeDispatch::new(vec![
        ok("alpha attempt"),
        ok("Merge readiness: DO_NOT_MERGE"),
        // beta dispatch should NOT happen — runner should short-
        // circuit on failed dependency. If it does dispatch, the
        // FakeDispatch will run out of canned outcomes and return Err.
    ]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(TWO_MILESTONE_TOML), state_root.path(), &dispatcher, repo.path())
            .unwrap();
    assert_eq!(summary.exit_code, 2);
    let beta = summary.outcomes.iter().find(|o| o.id == "beta").unwrap();
    assert_eq!(beta.final_state, "FAILED");
    assert_eq!(beta.fix_loop_iterations, 0, "beta must not have run an implementer");
    assert_eq!(dispatcher.calls().len(), 2, "only alpha's two calls");
}

// ─── unparseable verdict triggers fallback (treated as NeedsFix) ─

#[test]
fn unparseable_verdict_is_treated_as_needs_fix_for_fix_loop() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "unparseable-test"
target_branch = "main"

[defaults]
fix_loop_max = 2

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    let dispatcher = FakeDispatch::new(vec![
        ok("attempt 1"),
        ok("the reviewer forgot the verdict line"),
        ok("attempt 2"),
        ok("Merge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    assert_eq!(summary.exit_code, 0);
    assert_eq!(summary.outcomes[0].final_state, "MERGED");
    // First iteration's unparseable verdict counted as needs-fix
    // and triggered iter 2.
    assert_eq!(summary.outcomes[0].fix_loop_iterations, 2);
}

// ─── implementer dispatch failure → FAILED ───────────────────────

#[test]
fn implementer_dispatch_failure_marks_failed() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "imp-fail-test"
target_branch = "main"

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    let dispatcher = FakeDispatch::new(vec![fail("oom")]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    assert_eq!(summary.exit_code, 2);
    assert_eq!(summary.outcomes[0].final_state, "FAILED");
    // Reviewer was never called.
    assert_eq!(dispatcher.calls().len(), 1);
}

// ─── per-milestone fix_loop_max overrides default ─────────────────

#[test]
fn per_milestone_fix_loop_max_overrides_default() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "override-test"
target_branch = "main"

[defaults]
fix_loop_max = 5

[[milestones]]
id = "alpha"
branch = "feat/alpha"
fix_loop_max = 1
implementer = "router-implementer"
assignment = "do alpha"
"#;
    // One implementer + one reviewer with NEEDS_FIX → exhausted (max=1).
    let dispatcher = FakeDispatch::new(vec![
        ok("attempt 1"),
        ok("Merge readiness: NEEDS_FIX"),
    ]);
    let state_root = tempdir().unwrap();
    let summary =
        run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    assert_eq!(summary.exit_code, 2);
    assert_eq!(summary.outcomes[0].final_state, "FAILED");
    assert_eq!(summary.outcomes[0].fix_loop_iterations, 1);
}

// ─── per-milestone reviewer override ─────────────────────────────

#[test]
fn per_milestone_reviewer_override_used() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "reviewer-override-test"
target_branch = "main"

[defaults]
reviewer = "code-reviewer"

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
reviewer = "rfd-critic"
assignment = "do alpha"
"#;
    let dispatcher = FakeDispatch::new(vec![
        ok("attempt 1"),
        ok("Merge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    run_with(&parse(toml), state_root.path(), &dispatcher, repo.path()).unwrap();
    let calls = dispatcher.calls();
    assert!(matches!(calls[0].0, DispatchRole::Implementer));
    assert_eq!(calls[0].1, "router-implementer");
    assert!(matches!(calls[1].0, DispatchRole::Reviewer));
    assert_eq!(calls[1].1, "rfd-critic", "reviewer override should be used");
}
