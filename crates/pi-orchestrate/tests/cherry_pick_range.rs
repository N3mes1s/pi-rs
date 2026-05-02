//! Regression tests for the multi-iter cherry-pick range bug.
//!
//! Background: orchestrate v1 cherry-picked **only the milestone
//! branch's tip** onto `target_branch`. When the implementer
//! committed a fix-up on iter 2 (in response to the reviewer's
//! `NEEDS_FIX`), the tip was a small fix-up commit whose patch
//! depended on iter 1 already being on disk. Cherry-picking just
//! the tip on top of `target_branch` (which doesn't have iter 1
//! yet) conflicted, marking every multi-iter milestone
//! `BLOCKED_ON_CONFLICT`.
//!
//! Fix: cherry-pick the **range** `target_head_at_review..branch_sha`,
//! which applies every commit on the milestone branch that's not
//! already on target — including all fix-up commits. See
//! `pi-orchestrate/src/merge.rs §"Why range, not tip-only"`.
//!
//! The tests below build a repo where the implementer made TWO
//! commits on the feature branch (iter 1 main work + iter 2 fix-up).
//! With the old tip-only behavior, the cherry-pick would conflict.
//! With the new range behavior, both commits land cleanly.

use pi_orchestrate::{cherry_pick_to_target, MergeOutcome};
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

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

fn run_capture(p: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(p)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {:?} failed", args);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Build a repo with the structure:
///
/// ```
///   main → A (base commit)
///   feat → A → I1 (iter 1: large refactor) → I2 (iter 2: fix-up dep on I1)
/// ```
///
/// I2's patch alone, applied to `main` (which lacks I1), would
/// reference deleted/changed code that's not there yet — conflict.
/// The range I1..I2 is what we want to apply.
fn make_repo_two_iters() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let p = dir.path();
    run(p, &["init", "-q", "-b", "main"]);
    run(p, &["config", "user.email", "test@example.com"]);
    run(p, &["config", "user.name", "Test"]);
    run(p, &["config", "commit.gpgsign", "false"]);
    run(p, &["config", "tag.gpgsign", "false"]);
    // Base commit on main.
    std::fs::write(p.join("README.md"), "base\n").unwrap();
    run(p, &["add", "README.md"]);
    run(p, &["commit", "-q", "-m", "base"]);

    // feat branch off main.
    run(p, &["checkout", "-q", "-b", "feat"]);

    // I1: iter 1 — implementer creates a new file.
    std::fs::write(p.join("module.rs"), "pub fn one() {}\n").unwrap();
    run(p, &["add", "module.rs"]);
    run(p, &["commit", "-q", "-m", "iter 1: add module"]);

    // I2: iter 2 — fix-up that EDITS the file I1 created. Without
    // I1 on the target, `pub fn one() {}` doesn't exist to edit.
    std::fs::write(p.join("module.rs"), "pub fn one() {}\npub fn two() {}\n").unwrap();
    run(p, &["add", "module.rs"]);
    run(p, &["commit", "-q", "-m", "iter 2: add second function"]);

    dir
}

#[test]
fn cherry_pick_range_applies_both_iter_commits_to_target() {
    let repo = make_repo_two_iters();
    let p = repo.path();

    let target_head = run_capture(p, &["rev-parse", "main"]);
    let feat_tip = run_capture(p, &["rev-parse", "feat"]);

    // Sanity: feat is two commits ahead of main.
    let count = run_capture(p, &["rev-list", "--count", "main..feat"]);
    assert_eq!(count, "2", "expected feat to have 2 commits ahead of main");

    let outcome = cherry_pick_to_target(p, "main", &target_head, &feat_tip);
    assert_eq!(outcome, MergeOutcome::Merged);

    // Both commits should now be on main.
    let count_after = run_capture(p, &["rev-list", "--count", "main"]);
    let initial = run_capture(p, &["rev-list", "--count", &target_head]);
    let landed: u32 = count_after.parse().unwrap();
    let before: u32 = initial.parse().unwrap();
    assert_eq!(
        landed - before,
        2,
        "expected 2 new commits on main after range cherry-pick, got {}",
        landed - before
    );

    // The file content should match feat's tip — both functions present.
    let content = std::fs::read_to_string(p.join("module.rs")).unwrap();
    assert!(content.contains("pub fn one()"), "iter 1 missing");
    assert!(content.contains("pub fn two()"), "iter 2 missing");
}

#[test]
fn cherry_pick_tip_only_would_conflict_proves_range_is_required() {
    // This test exists to document why the range form is the fix:
    // the OLD `git cherry-pick <tip>` (passing only feat_tip without
    // a base) on a target lacking iter 1 would conflict because
    // iter 2's patch references iter 1's content.
    let repo = make_repo_two_iters();
    let p = repo.path();

    let feat_tip = run_capture(p, &["rev-parse", "feat"]);

    run(p, &["checkout", "-q", "main"]);

    let cp = Command::new("git")
        .args(["cherry-pick", &feat_tip])
        .current_dir(p)
        .output()
        .unwrap();

    assert!(
        !cp.status.success(),
        "tip-only cherry-pick should fail when iter 1 isn't on target — \
         this is the bug the range form fixes"
    );
    let stderr = String::from_utf8_lossy(&cp.stderr);
    assert!(
        stderr.contains("CONFLICT") || stderr.contains("could not apply"),
        "expected conflict marker in stderr, got: {stderr}"
    );

    // Cleanup so the test doesn't leave a half-applied state.
    let _ = Command::new("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(p)
        .output();
}

#[test]
fn cherry_pick_range_single_commit_still_works() {
    // Sanity: when the implementer made only ONE commit on feat
    // (no fix-up needed), the range form should still apply that
    // single commit. Range = single = same outcome.
    let dir = tempdir().unwrap();
    let p = dir.path();
    run(p, &["init", "-q", "-b", "main"]);
    run(p, &["config", "user.email", "test@example.com"]);
    run(p, &["config", "user.name", "Test"]);
    run(p, &["config", "commit.gpgsign", "false"]);
    run(p, &["config", "tag.gpgsign", "false"]);
    std::fs::write(p.join("README.md"), "base\n").unwrap();
    run(p, &["add", "README.md"]);
    run(p, &["commit", "-q", "-m", "base"]);

    run(p, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(p.join("new.rs"), "// added\n").unwrap();
    run(p, &["add", "new.rs"]);
    run(p, &["commit", "-q", "-m", "single iter commit"]);

    let target_head = run_capture(p, &["rev-parse", "main"]);
    let feat_tip = run_capture(p, &["rev-parse", "feat"]);

    let outcome = cherry_pick_to_target(p, "main", &target_head, &feat_tip);
    assert_eq!(outcome, MergeOutcome::Merged);

    assert!(p.join("new.rs").exists());
}
