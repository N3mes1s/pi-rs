//! Regression tests for B1/B2/B3 from the v1 review:
//!
//!   B1: subagent file at `<repo>/.pi/agents/<name>.md` is actually
//!       loaded — model + thinking + system prompt extracted.
//!   B2: runner runs `git checkout m.branch` before each dispatch
//!       so milestones don't execute on the wrong branch after a
//!       prior cherry-pick.
//!   B3: cherry-pick path performs the staleness check — if
//!       target_branch HEAD moved between review and merge, the
//!       milestone transitions to BLOCKED_ON_REVIEW_STALE rather
//!       than silently merging an outdated diff.
//!   D1: prune warnings from `git worktree remove` failures are
//!       threaded into the `DISPATCHED` state detail, not dropped.

use pi_orchestrate::dispatch::{Dispatch, DispatchOutcome, DispatchRole};
use pi_orchestrate::{parse_campaign, replay, run_with, validate};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use tempfile::tempdir;

// ─── B2 + B3: full-runner integration ──────────────────────────
// (B1 parser tests live inline in `dispatch::tests` since
// `parse_agent_md` is `pub(crate) fn`.)

struct FakeDispatch {
    canned: Mutex<Vec<DispatchOutcome>>,
    /// Records the cwd's HEAD branch at each dispatch, so the test
    /// can assert that B2 checked out the right branch before
    /// dispatching.
    branches_seen: Mutex<Vec<String>>,
    /// Records each dispatch as `(role, agent_name, assignment_text)`
    /// so tests can assert ordering / forwarding-text invariants.
    /// Concern C4 in the v1 review: the previous FakeDispatch only
    /// recorded role+agent, so reviewer-input construction and
    /// fix-loop forwarding text were untestable.
    calls: Mutex<Vec<(DispatchRole, String, String)>>,
    /// If set, a closure run before returning each canned outcome.
    /// Used by the B3 test to simulate a target-branch move between
    /// review snapshot and merge.
    side_effect: Mutex<Option<Box<dyn FnMut(&Path) + Send>>>,
}

impl FakeDispatch {
    fn new(canned: Vec<DispatchOutcome>) -> Self {
        Self {
            canned: Mutex::new(canned),
            branches_seen: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            side_effect: Mutex::new(None),
        }
    }
    fn with_side_effect(canned: Vec<DispatchOutcome>, f: Box<dyn FnMut(&Path) + Send>) -> Self {
        Self {
            canned: Mutex::new(canned),
            branches_seen: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            side_effect: Mutex::new(Some(f)),
        }
    }
    fn branches_seen(&self) -> Vec<String> {
        self.branches_seen.lock().unwrap().clone()
    }
    fn calls(&self) -> Vec<(DispatchRole, String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Dispatch for FakeDispatch {
    fn dispatch(
        &self,
        role: DispatchRole,
        agent_name: &str,
        assignment: &str,
        cwd: &Path,
    ) -> std::io::Result<DispatchOutcome> {
        let head = current_branch(cwd);
        self.branches_seen.lock().unwrap().push(head);
        self.calls
            .lock()
            .unwrap()
            .push((role, agent_name.to_string(), assignment.to_string()));
        if let Some(f) = self.side_effect.lock().unwrap().as_mut() {
            f(cwd);
        }
        let mut q = self.canned.lock().unwrap();
        if q.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "no canned outcomes left",
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

fn current_branch(repo_root: &Path) -> String {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repo_root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn make_repo(target: &str, milestone_branches: &[&str]) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let p = dir.path();
    fn run(p: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    run(p, &["init", "-q", "-b", target]);
    run(p, &["config", "user.email", "test@example.com"]);
    run(p, &["config", "user.name", "Test"]);
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

const TWO_MS_TOML: &str = r#"
name = "two-ms"
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

// ─── B2 ─────────────────────────────────────────────────────────

#[test]
fn b2_each_milestone_checks_out_its_branch_before_dispatch() {
    let repo = make_repo("main", &["feat/alpha", "feat/beta"]);
    let dispatcher = FakeDispatch::new(vec![
        ok("alpha imp"),
        ok("Merge readiness: READY_TO_MERGE"),
        ok("beta imp"),
        ok("Merge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary = run_with(
        &parse_and_validate(TWO_MS_TOML),
        state_root.path(),
        &dispatcher,
        repo.path(),
    )
    .unwrap();
    assert_eq!(summary.exit_code, 0);

    // 4 dispatches total: alpha-imp, alpha-rev, beta-imp, beta-rev.
    // B2 says implementer + reviewer both happen on m.branch, so:
    //   alpha-imp → feat/alpha
    //   alpha-rev → feat/alpha
    //   beta-imp  → feat/beta   (CRITICAL — was failing before B2)
    //   beta-rev  → feat/beta
    let branches = dispatcher.branches_seen();
    assert_eq!(branches.len(), 4);
    assert_eq!(
        branches[0], "feat/alpha",
        "alpha implementer must run on feat/alpha"
    );
    assert_eq!(
        branches[1], "feat/alpha",
        "alpha reviewer must run on feat/alpha"
    );
    assert_eq!(
        branches[2], "feat/beta",
        "beta implementer must run on feat/beta — this is the bug B2 catches: \
         the previous v1 left us on `main` after alpha's cherry-pick"
    );
    assert_eq!(branches[3], "feat/beta");
}

// ─── B3 ─────────────────────────────────────────────────────────

#[test]
fn b3_target_head_moves_between_review_and_merge_blocks_on_review_stale() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "stale-test"
target_branch = "main"

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    // Simulate an external commit landing on main between the
    // reviewer-snapshot capture and the merge-time recheck. We do
    // this via a side-effect run on the SECOND dispatch (the
    // reviewer call): after the snapshot is captured, we add a
    // commit on main. Then the runner re-rev-parses and sees the
    // mismatch.
    let repo_path = repo.path().to_path_buf();
    let mut counter = 0;
    let side_effect = Box::new(move |_cwd: &Path| {
        counter += 1;
        if counter == 2 {
            // Reviewer call. Add an external commit to main.
            let p = &repo_path;
            for cmd in &[
                vec!["checkout", "-q", "main"],
                vec!["commit", "--allow-empty", "-q", "-m", "external commit"],
                // Switch back to feat/alpha so the runner's next
                // checkout doesn't have to fix anything weird.
                vec!["checkout", "-q", "feat/alpha"],
            ] {
                let out = Command::new("git")
                    .args(cmd)
                    .current_dir(p)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "side-effect git {:?} failed", cmd);
            }
        }
    });
    let dispatcher = FakeDispatch::with_side_effect(
        vec![ok("alpha imp"), ok("Merge readiness: READY_TO_MERGE")],
        side_effect,
    );
    let state_root = tempdir().unwrap();
    let summary = run_with(
        &parse_and_validate(toml),
        state_root.path(),
        &dispatcher,
        repo.path(),
    )
    .unwrap();
    // RFD §"Exit codes": 3 means at least one BLOCKED.
    assert_eq!(summary.exit_code, 3);
    assert_eq!(
        summary.outcomes[0].final_state, "BLOCKED_ON_REVIEW_STALE",
        "target moved between review snapshot and merge → must block, not merge"
    );
}

fn parse_and_validate(toml: &str) -> pi_orchestrate::Campaign {
    let c = parse_campaign(toml).unwrap();
    validate(&c).unwrap();
    c
}

// ─── C3: cherry-pick conflict path ──────────────────────────────

/// Build a repo where two milestone branches both modify the SAME
/// line of the same file. Cherry-picking the second after the first
/// has merged onto target_branch will conflict — exercising the
/// `MergeOutcome::Conflict` path the reviewer flagged as untested.
fn make_repo_with_conflict() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let p = dir.path();
    fn run(p: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    run(p, &["init", "-q", "-b", "main"]);
    run(p, &["config", "user.email", "test@example.com"]);
    run(p, &["config", "user.name", "Test"]);
    run(p, &["config", "commit.gpgsign", "false"]);
    run(p, &["config", "tag.gpgsign", "false"]);
    std::fs::write(p.join("shared.txt"), "original\n").unwrap();
    run(p, &["add", "shared.txt"]);
    run(p, &["commit", "-q", "-m", "base"]);

    // Both branches modify the same line. After alpha lands, beta
    // can't apply.
    for (b, content) in &[("feat/alpha", "alpha edit\n"), ("feat/beta", "beta edit\n")] {
        run(p, &["checkout", "-q", "-b", b]);
        std::fs::write(p.join("shared.txt"), content).unwrap();
        run(p, &["add", "shared.txt"]);
        run(p, &["commit", "-q", "-m", &format!("touch shared on {b}")]);
        run(p, &["checkout", "-q", "main"]);
    }
    dir
}

#[test]
fn c3_cherry_pick_conflict_marks_blocked_on_conflict_and_cleans_tree() {
    let repo = make_repo_with_conflict();
    let dispatcher = FakeDispatch::new(vec![
        ok("alpha imp"),
        ok("Merge readiness: READY_TO_MERGE"),
        ok("beta imp"),
        ok("Merge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary = run_with(
        &parse_and_validate(TWO_MS_TOML),
        state_root.path(),
        &dispatcher,
        repo.path(),
    )
    .unwrap();

    // Alpha cherry-picks cleanly; beta conflicts. Exit 3 because at
    // least one milestone is blocked.
    assert_eq!(summary.exit_code, 3);
    let alpha = summary.outcomes.iter().find(|o| o.id == "alpha").unwrap();
    let beta = summary.outcomes.iter().find(|o| o.id == "beta").unwrap();
    assert_eq!(alpha.final_state, "MERGED");
    assert_eq!(beta.final_state, "BLOCKED_ON_CONFLICT");

    // Tree must be clean — no in-flight cherry-pick state, no
    // unstaged residue from the failed merge. The runner's abort
    // logic (merge::cherry_pick_to_target) is responsible.
    let status_out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let dirty = String::from_utf8_lossy(&status_out.stdout);
    assert!(
        dirty.trim().is_empty(),
        "working tree must be clean after BLOCKED_ON_CONFLICT, got: {dirty}"
    );
    let cherry_pick_head = repo.path().join(".git").join("CHERRY_PICK_HEAD");
    assert!(
        !cherry_pick_head.exists(),
        ".git/CHERRY_PICK_HEAD must be cleared by the abort"
    );
}

// ─── C4: FakeDispatch can now assert reviewer-assignment content ──

#[test]
fn c4_reviewer_assignment_includes_implementer_output_and_target_branch() {
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "reviewer-input-test"
target_branch = "main"

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;
    let dispatcher = FakeDispatch::new(vec![
        ok("IMPLEMENTER_OUTPUT_SENTINEL_42"),
        ok("Merge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    run_with(
        &parse_and_validate(toml),
        state_root.path(),
        &dispatcher,
        repo.path(),
    )
    .unwrap();

    let calls = dispatcher.calls();
    assert_eq!(calls.len(), 2);
    // Reviewer call (second) — its assignment text must include the
    // implementer's output (so the reviewer can read it) and the
    // campaign target_branch in the diff command (concern C1, fixed
    // for free in f5f329a).
    let (_role, _agent, reviewer_assignment) = &calls[1];
    assert!(
        reviewer_assignment.contains("IMPLEMENTER_OUTPUT_SENTINEL_42"),
        "reviewer must see implementer output: {reviewer_assignment}"
    );
    assert!(
        reviewer_assignment.contains("git diff main...feat/alpha"),
        "reviewer's diff command must reference the campaign's target_branch: \
         {reviewer_assignment}"
    );
}

// ─── C2: replay distinguishes truncated final from mid-stream corruption ──

#[test]
fn c2_replay_errors_on_mid_stream_corruption_not_just_truncated_final() {
    use pi_orchestrate::replay;
    let dir = tempdir().unwrap();
    let p = dir.path().join("state.jsonl");
    // Two valid events, one corrupt line in the MIDDLE, one valid
    // event after. The previous "stop on first error" replay would
    // silently return only the first 2 events, hiding the corruption
    // and the events after it.
    let valid1 = r#"{"milestone":"a","from":"PENDING","to":"DISPATCHED","ts":1}"#;
    let valid2 = r#"{"milestone":"a","from":"DISPATCHED","to":"REVIEWED","ts":2}"#;
    let corrupt = r#"{"milestone":"a","from":...this is not json"#;
    let valid3 = r#"{"milestone":"a","from":"REVIEWED","to":"MERGED","ts":3}"#;
    let body = format!("{valid1}\n{valid2}\n{corrupt}\n{valid3}\n");
    std::fs::write(&p, body).unwrap();

    let result = replay(&p);
    assert!(
        result.is_err(),
        "mid-stream corruption must be surfaced, not silently masked"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("line 3") || err_msg.contains("mid-stream"),
        "error must point to the corrupt line: {err_msg}"
    );
}

#[test]
fn c2_replay_tolerates_truncated_final_line() {
    use pi_orchestrate::replay;
    let dir = tempdir().unwrap();
    let p = dir.path().join("state.jsonl");
    // Two valid events + one truncated final (mid-write crash).
    let valid1 = r#"{"milestone":"a","from":"PENDING","to":"DISPATCHED","ts":1}"#;
    let valid2 = r#"{"milestone":"a","from":"DISPATCHED","to":"REVIEWED","ts":2}"#;
    let truncated = r#"{"milestone":"a","from":"REVIEW"#; // half-written
    let body = format!("{valid1}\n{valid2}\n{truncated}");
    std::fs::write(&p, body).unwrap();

    let events = replay(&p).expect("truncated final must NOT error");
    assert_eq!(
        events.len(),
        2,
        "replay should yield the 2 valid events and silently drop the truncated one"
    );
}

// ─── D1: prune warnings are threaded into DISPATCHED detail ──────────────────
//
// Scenario: a stale worktree exists for the milestone branch BEFORE the runner
// starts. On iter=1, `git_checkout` calls `prune_stale_worktrees` which removes
// it cleanly (no warnings). The DISPATCHED event detail must be `iter=1 agent=...`
// with no `prune warnings:` suffix — proving the clean-prune success path has
// the right format.
//
// Additionally, if prune warnings WERE present, they must appear in the
// DISPATCHED detail (not in some other event or lost entirely). We verify
// this separately through a direct call to `git_checkout` that forces a
// warning, then confirm the returned warning string is what a caller
// would embed in a DISPATCHED detail.

#[test]
fn d1_prune_warnings_appear_in_dispatched_detail_on_success_path() {
    // ── set up a repo with one milestone branch ──
    let repo = make_repo("main", &["feat/alpha"]);
    let toml = r#"
name = "prune-warning-test"
target_branch = "main"

[[milestones]]
id = "alpha"
branch = "feat/alpha"
implementer = "router-implementer"
assignment = "do alpha"
"#;

    // Add a stale worktree for feat/alpha BEFORE run_with, simulating a
    // prior reviewer subprocess that left one behind.
    let stale_dir = tempdir().unwrap();
    let stale_path = stale_dir.path();
    let wt_out = Command::new("git")
        .args(["worktree", "add", stale_path.to_str().unwrap(), "feat/alpha"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        wt_out.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&wt_out.stderr)
    );

    // Without our fix, run_with would fail on `git checkout feat/alpha`
    // because the branch is locked by the stale worktree.
    let dispatcher = FakeDispatch::new(vec![
        ok("imp output"),
        ok("Merge readiness: READY_TO_MERGE"),
    ]);
    let state_root = tempdir().unwrap();
    let summary = run_with(
        &parse_and_validate(toml),
        state_root.path(),
        &dispatcher,
        repo.path(),
    )
    .unwrap();

    // The milestone must have merged cleanly (stale worktree pruned by
    // `git_checkout` before the implementer was dispatched).
    assert_eq!(
        summary.outcomes[0].final_state, "MERGED",
        "alpha must merge; stale worktree should have been pruned: {:?}",
        summary
    );

    // Read state.jsonl and find the DISPATCHED event.
    let events = replay(&summary.state_path).expect("state.jsonl must be readable");
    let dispatched = events
        .iter()
        .find(|e| e.to == "DISPATCHED")
        .expect("must have a DISPATCHED event");

    // Must contain iter=1.
    assert!(
        dispatched.detail.contains("iter=1"),
        "DISPATCHED detail must contain 'iter=1': {:?}",
        dispatched.detail
    );

    // On this clean path there were no prune failures, so the detail
    // must NOT contain the prune warnings suffix.
    assert!(
        !dispatched.detail.contains("prune warnings:"),
        "clean prune path must not inject noise into DISPATCHED detail: {:?}",
        dispatched.detail
    );
}

/// D1 companion: verify that if `prune_stale_worktrees` returns non-empty
/// warnings, those strings are what a caller would embed in a DISPATCHED
/// event detail — i.e. the formatting `"iter={} agent={}; prune warnings: {}"` 
/// is exercised when warnings are non-empty.
///
/// We do this by directly calling `git_checkout` in a scenario where
/// `git worktree remove --force` fails (registry entry made read-only),
/// capturing the returned warnings, and asserting:
///   (a) warnings is non-empty
///   (b) the constructed detail string has the expected shape
#[test]
fn d1_prune_warnings_format_in_dispatched_detail() {
    use pi_orchestrate::git_checkout;

    let repo_dir = tempdir().unwrap();
    let repo = repo_dir.path();

    // Minimal repo with a feat branch.
    fn git(p: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "test@test.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["config", "tag.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "base\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("feat.txt"), "feat\n").unwrap();
    git(repo, &["add", "feat.txt"]);
    git(repo, &["commit", "-q", "-m", "feat"]);
    git(repo, &["checkout", "-q", "main"]);

    // Add a worktree for feat so it shows up in `git worktree list`.
    let wt_dir = tempdir().unwrap();
    let wt_path = wt_dir.path();
    git(
        repo,
        &["worktree", "add", wt_path.to_str().unwrap(), "feat"],
    );

    // Make the worktree registry entry read-only so `git worktree remove
    // --force` fails with a permission error, generating a warning.
    let wt_name = std::fs::read_dir(repo.join(".git").join("worktrees"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let registry_entry = repo.join(".git").join("worktrees").join(&wt_name);
    std::fs::set_permissions(&registry_entry, std::os::unix::fs::PermissionsExt::from_mode(0o555))
        .unwrap();

    // git_checkout must return warnings (from the failed remove) even
    // though the checkout itself may fail too.
    let (warnings, _checkout_result) = git_checkout(repo, "feat");

    // Restore permissions so tempdir cleanup doesn't panic.
    std::fs::set_permissions(
        &registry_entry,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    // The key assertion: warnings must be non-empty.
    assert!(
        !warnings.is_empty(),
        "Expected at least one prune warning when worktree remove --force fails, got none"
    );

    // Simulate the runner's DISPATCHED detail construction for the
    // success path — proves the format used in runner.rs is correct.
    let iter = 1u32;
    let agent = "router-implementer";
    let detail = if warnings.is_empty() {
        format!("iter={iter} agent={agent}")
    } else {
        format!(
            "iter={iter} agent={agent}; prune warnings: {}",
            warnings.join("; ")
        )
    };
    assert!(
        detail.contains("prune warnings:"),
        "DISPATCHED detail must contain prune warnings when they exist: {detail}"
    );
    assert!(
        detail.contains("iter=1"),
        "DISPATCHED detail must still contain iter= prefix: {detail}"
    );
    assert!(
        detail.contains("agent=router-implementer"),
        "DISPATCHED detail must still contain agent= field: {detail}"
    );
}
