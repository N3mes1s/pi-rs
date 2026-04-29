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

use pi_orchestrate::dispatch::{Dispatch, DispatchOutcome, DispatchRole};
use pi_orchestrate::{parse_campaign, run_with, validate};
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
            side_effect: Mutex::new(None),
        }
    }
    fn with_side_effect(
        canned: Vec<DispatchOutcome>,
        f: Box<dyn FnMut(&Path) + Send>,
    ) -> Self {
        Self {
            canned: Mutex::new(canned),
            branches_seen: Mutex::new(Vec::new()),
            side_effect: Mutex::new(Some(f)),
        }
    }
    fn branches_seen(&self) -> Vec<String> {
        self.branches_seen.lock().unwrap().clone()
    }
}

impl Dispatch for FakeDispatch {
    fn dispatch(
        &self,
        _role: DispatchRole,
        _agent_name: &str,
        _assignment: &str,
        cwd: &Path,
    ) -> std::io::Result<DispatchOutcome> {
        let head = current_branch(cwd);
        self.branches_seen.lock().unwrap().push(head);
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
        let out = Command::new("git").args(args).current_dir(p).output().unwrap();
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
    assert_eq!(branches[0], "feat/alpha", "alpha implementer must run on feat/alpha");
    assert_eq!(branches[1], "feat/alpha", "alpha reviewer must run on feat/alpha");
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
                let out = Command::new("git").args(cmd).current_dir(p).output().unwrap();
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
