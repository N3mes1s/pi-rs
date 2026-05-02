//! Cherry-pick a milestone branch onto the campaign target branch.
//!
//! RFD 0021 §"Merge queue" specs:
//!   * single-threaded merge worker (one merge at a time)
//!   * primitive is `git cherry-pick target_head..reviewed_branch_sha`
//!     — the **range** form, not the tip-only form
//!   * staleness check: compare current `git rev-parse <target>` to
//!     the `reviewed_target_head_sha` snapshot taken at review time;
//!     mismatch → `BLOCKED_ON_REVIEW_STALE`
//!
//! ### Why range, not tip-only
//!
//! The implementer's branch typically accumulates multiple commits
//! across the fix-loop iterations: iter 1 lays down the main work,
//! iter 2+ are fix-up commits in response to the reviewer's
//! `NEEDS_FIX` deltas. A tip-only `git cherry-pick branch_sha`
//! tries to apply only the LAST commit on top of `target_branch`,
//! and that commit (a fix-up) typically references context laid down
//! by earlier iter commits — so it conflicts. The range form
//! `git cherry-pick target_head..branch_sha` applies every commit
//! in the milestone branch's history that's not already on target,
//! in order, reproducing the implementer's full sequence on target.
//!
//! This was historically a recurring failure mode (BLOCKED_ON_CONFLICT
//! on every multi-iter milestone, requiring manual cherry-pick recovery
//! by the operator). The fix below picks the whole range.
//!
//! v1 implements all three. The staleness check happens in the
//! runner (`runner::run_with`), so this module is invoked only after
//! the runner has confirmed `target_head` hasn't moved since the
//! review snapshot. Cherry-pick failures are categorised by parsing
//! `git cherry-pick`'s stderr: a conflict (string `CONFLICT` /
//! `could not apply`) → `MergeOutcome::Conflict`; anything else
//! (e.g. unknown ref, missing object) → `MergeOutcome::GitError`.
//! On any non-success path the cherry-pick is aborted; if `--abort`
//! itself fails, that's surfaced as a `GitError` so the operator
//! sees a dirty tree rather than silently moving on.

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    Conflict,
    GitError(String),
}

/// `git checkout <branch>` in the given repo. Used by the runner to
/// switch between milestone branches and the campaign target branch.
/// Bug B2 in the v1 review: the runner never checked out
/// `m.branch` before dispatch, so post-merge milestones executed on
/// `target_branch`.
pub fn git_checkout(repo_root: &Path, branch: &str) -> std::io::Result<()> {
    let out = Command::new("git")
        .args(["checkout", "-q", branch])
        .current_dir(repo_root)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(
            format!(
                "git checkout {branch} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(())
}

/// Resolve `git rev-parse <ref>` in the given repo.
pub fn rev_parse(repo_root: &Path, refname: &str) -> std::io::Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", refname])
        .current_dir(repo_root)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(
            format!(
                "git rev-parse {refname} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Cherry-pick the range `target_head_at_review..branch_sha` onto
/// `target_branch` (which is checked out in `repo_root`). The range
/// form picks every commit on the milestone branch that's not yet on
/// target — including all fix-up commits from later fix-loop
/// iterations, not just the tip. On conflict, abort the cherry-pick
/// so the working tree is clean for the next milestone.
///
/// `target_head_at_review` is the SHA of `target_branch`'s HEAD at
/// the moment the reviewer signed off (recorded in the runner's
/// REVIEWED → MERGE_PENDING event). Passing this explicitly avoids a
/// race where target moves between review and merge — the staleness
/// check upstream catches that case.
pub fn cherry_pick_to_target(
    repo_root: &Path,
    target_branch: &str,
    target_head_at_review: &str,
    branch_sha: &str,
) -> MergeOutcome {
    // 1. Switch to target_branch.
    let checkout = Command::new("git")
        .args(["checkout", target_branch])
        .current_dir(repo_root)
        .output();
    let Ok(co) = checkout else {
        return MergeOutcome::GitError(format!("git checkout {target_branch}: spawn failed"));
    };
    if !co.status.success() {
        return MergeOutcome::GitError(format!(
            "git checkout {target_branch} failed: {}",
            String::from_utf8_lossy(&co.stderr).trim()
        ));
    }

    // 2. Cherry-pick the range. `target_head..branch_sha` expands to
    //    every commit reachable from branch_sha but not from target,
    //    applied in topological order. Equivalent to running
    //    `git cherry-pick A B C ...` for each iter's commit.
    let range = format!("{target_head_at_review}..{branch_sha}");
    let cp = Command::new("git")
        .args(["cherry-pick", &range])
        .current_dir(repo_root)
        .output();
    let Ok(cp) = cp else {
        return MergeOutcome::GitError("git cherry-pick: spawn failed".into());
    };
    if cp.status.success() {
        return MergeOutcome::Merged;
    }

    // 3. Cherry-pick failed. Categorise: conflict vs other git error.
    //    Concern C3 from the v1 review: previously every failure was
    //    treated as Conflict, which misreports state when the actual
    //    cause is e.g. an unknown ref or a corrupted object.
    //    `git cherry-pick`'s stderr contains "CONFLICT" or
    //    "could not apply" on a real merge conflict; anything else
    //    bubbles up as GitError so the operator sees the real cause.
    let stderr = String::from_utf8_lossy(&cp.stderr);
    let is_conflict = stderr.contains("CONFLICT") || stderr.contains("could not apply");

    // 4. Abort the in-progress cherry-pick to clean the tree. If
    //    the abort itself fails, surface that as GitError — leaving
    //    a dirty tree and returning Conflict would silently break
    //    the next milestone's `git checkout`.
    let abort = Command::new("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(repo_root)
        .output();
    if let Ok(a) = &abort {
        if !a.status.success() {
            return MergeOutcome::GitError(format!(
                "cherry-pick failed AND --abort failed: cp_stderr={} abort_stderr={}",
                stderr.trim(),
                String::from_utf8_lossy(&a.stderr).trim()
            ));
        }
    } else {
        return MergeOutcome::GitError(
            "cherry-pick failed AND `git cherry-pick --abort` could not be spawned".into(),
        );
    }

    if is_conflict {
        MergeOutcome::Conflict
    } else {
        MergeOutcome::GitError(format!("git cherry-pick failed: {}", stderr.trim()))
    }
}
