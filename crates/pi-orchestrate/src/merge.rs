//! Cherry-pick a milestone branch onto the campaign target branch.
//!
//! RFD 0021 §"Merge queue" specs:
//!   * single-threaded merge worker (one merge at a time)
//!   * primitive is `git cherry-pick reviewed_branch_sha`
//!   * staleness check: compare current `git rev-parse <target>` to
//!     the `reviewed_target_head_sha` snapshot taken at review time;
//!     mismatch → `BLOCKED_ON_REVIEW_STALE` (v3, not v1)
//!
//! v1 implements the simple cherry-pick + conflict-detection path.
//! Staleness detection is wired in (we already capture
//! reviewed_target_head_sha in the state event), but on mismatch v1
//! still attempts the cherry-pick — Git itself will produce a conflict
//! if the bases diverged enough to matter. v3 will lift the staleness
//! guard before the cherry-pick.

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    Conflict,
    GitError(String),
}

/// Resolve `git rev-parse <ref>` in the given repo.
pub fn rev_parse(repo_root: &Path, refname: &str) -> std::io::Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", refname])
        .current_dir(repo_root)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "git rev-parse {refname} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Cherry-pick `branch_sha` onto `target_branch` (which is checked out
/// in `repo_root`). On conflict, abort the cherry-pick so the working
/// tree is clean for the next milestone — we don't leave a half-
/// applied state for the operator to clean up.
pub fn cherry_pick_to_target(
    repo_root: &Path,
    target_branch: &str,
    branch_sha: &str,
) -> MergeOutcome {
    // 1. Switch to target_branch.
    let checkout = Command::new("git")
        .args(["checkout", target_branch])
        .current_dir(repo_root)
        .output();
    let Ok(co) = checkout else {
        return MergeOutcome::GitError(format!(
            "git checkout {target_branch}: spawn failed"
        ));
    };
    if !co.status.success() {
        return MergeOutcome::GitError(format!(
            "git checkout {target_branch} failed: {}",
            String::from_utf8_lossy(&co.stderr).trim()
        ));
    }

    // 2. Cherry-pick.
    let cp = Command::new("git")
        .args(["cherry-pick", branch_sha])
        .current_dir(repo_root)
        .output();
    let Ok(cp) = cp else {
        return MergeOutcome::GitError("git cherry-pick: spawn failed".into());
    };
    if cp.status.success() {
        return MergeOutcome::Merged;
    }

    // 3. Cherry-pick failed — likely a merge conflict. Abort it so
    //    the working tree returns to a clean state. The campaign
    //    flags this milestone as BLOCKED_ON_CONFLICT and continues
    //    with downstream milestones whose dependencies are still
    //    satisfiable. Recovery is manual (RFD §"Operator recovery").
    let _ = Command::new("git")
        .args(["cherry-pick", "--abort"])
        .current_dir(repo_root)
        .output();
    MergeOutcome::Conflict
}
