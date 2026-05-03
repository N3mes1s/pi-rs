//! Regression test for the stale-worktree checkout failure observed on
//! 2026-05-04 in campaign `sdk-bedrock-azure-streaming-timeout`.
//!
//! Scenario:
//!   A reviewer subprocess checked out branch `feat` in an external
//!   worktree. The session ended (worktree directory still exists on disk)
//!   and the runner tried to `git checkout feat` in the main repo, which
//!   git refuses because the branch is already checked out in another
//!   worktree.
//!
//! The fix: `git_checkout` now calls `prune_stale_worktrees` first,
//! which removes any registered worktrees that have the target branch
//! checked out before the actual checkout is attempted.

use pi_orchestrate::{git_checkout, prune_stale_worktrees};
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

/// Set up a minimal git repo at `p` with one commit on `branch_name`
/// branching off `main`.
fn make_repo_with_branch(p: &Path, branch_name: &str) {
    fn run(p: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(p)
            .output()
            .unwrap();
        if !out.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    run(p, &["init", "-q", "-b", "main"]);
    run(p, &["config", "user.email", "test@example.com"]);
    run(p, &["config", "user.name", "Test"]);
    run(p, &["config", "commit.gpgsign", "false"]);
    run(p, &["config", "tag.gpgsign", "false"]);
    std::fs::write(p.join("README.md"), "base\n").unwrap();
    run(p, &["add", "README.md"]);
    run(p, &["commit", "-q", "-m", "base"]);
    // Create feature branch.
    run(p, &["checkout", "-q", "-b", branch_name]);
    std::fs::write(p.join("feat.txt"), "feature\n").unwrap();
    run(p, &["add", "feat.txt"]);
    run(p, &["commit", "-q", "-m", "feat"]);
    // Return to main so the feature branch is free.
    run(p, &["checkout", "-q", "main"]);
}

/// Verify that the branch is currently checked out at `repo_root`.
fn current_branch(repo_root: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ─── test: prune_stale_worktrees removes the leftover worktree ───────────────

#[test]
fn prune_removes_stale_worktree_and_checkout_succeeds() {
    let repo_dir = tempdir().unwrap();
    let repo = repo_dir.path();
    make_repo_with_branch(repo, "feat");

    // Simulate a reviewer that added a worktree for `feat` and left it
    // behind (directory still exists on disk).
    let stale_dir = tempdir().unwrap();
    let stale = stale_dir.path();
    let wt_out = Command::new("git")
        .args(["worktree", "add", stale.to_str().unwrap(), "feat"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        wt_out.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&wt_out.stderr)
    );

    // Confirm that without cleanup, checkout fails.
    let co_before = Command::new("git")
        .args(["checkout", "-q", "feat"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        !co_before.status.success(),
        "Expected checkout to fail while worktree holds the branch"
    );

    // Exercise the public helper directly.
    let warnings = prune_stale_worktrees(repo, "feat");
    // Should have no warnings — the remove should succeed.
    assert!(
        warnings.is_empty(),
        "Expected no warnings from prune_stale_worktrees, got: {warnings:?}"
    );

    // The stale worktree directory's git metadata should be cleaned up.
    // Now checkout should succeed.
    git_checkout(repo, "feat").expect("git_checkout must succeed after prune");
    assert_eq!(current_branch(repo), "feat");
}

// ─── test: full git_checkout wrapper removes stale worktree ──────────────────

#[test]
fn git_checkout_succeeds_when_stale_worktree_exists() {
    let repo_dir = tempdir().unwrap();
    let repo = repo_dir.path();
    make_repo_with_branch(repo, "feat");

    // Add a stale worktree for `feat`.
    let stale_dir = tempdir().unwrap();
    let stale = stale_dir.path();
    let wt_out = Command::new("git")
        .args(["worktree", "add", stale.to_str().unwrap(), "feat"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        wt_out.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&wt_out.stderr)
    );

    // git_checkout should transparently clean up the worktree and
    // succeed, rather than returning an error.
    git_checkout(repo, "feat").expect("git_checkout should clean up stale worktree and succeed");
    assert_eq!(current_branch(repo), "feat", "HEAD must be on feat after checkout");

    // The stale worktree path still exists on disk (we hold a TempDir for it)
    // but git should no longer list it as an additional worktree — only the
    // main worktree (which is now on feat) should appear. We verify by
    // counting worktree blocks: there should be exactly one.
    let list_out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&list_out.stdout);
    let worktree_count = list
        .split("\n\n")
        .filter(|block| block.trim_start().starts_with("worktree "))
        .count();
    assert_eq!(
        worktree_count, 1,
        "only the main worktree should remain after prune; got:\n{list}"
    );
}
