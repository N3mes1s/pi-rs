//! Reconcile a finished worktree back into the parent repo as either a
//! cherry-picked branch commit or a `.patch` artifact.

use super::baseline::WorktreeBaseline;
use super::git::{self, GitError};
use super::patches_root;
use serde::Serialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub enum ReconcileMode {
    Branch,
    Patch,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReconcileOutcome {
    Branch {
        branch: String,
        sha: String,
        merged: bool,
    },
    Patch {
        path: PathBuf,
    },
    Empty,
    ConflictedBranch {
        branch: String,
    },
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("git error: {0}")]
    Git(#[from] GitError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn finish(
    repo_root: &Path,
    worktree: &Path,
    baseline: &WorktreeBaseline,
    task_id: &str,
    mode: ReconcileMode,
) -> Result<ReconcileOutcome, ReconcileError> {
    // `git add -A` then check whether anything is queued. Empty diff ⇒
    // shortcut.
    git::run(worktree, &["add", "-A"], "add -A").await?;
    let cached_diff = git::run(
        worktree,
        &["diff", "--cached", "--binary"],
        "diff --cached",
    )
    .await?;
    if cached_diff.is_empty() {
        return Ok(ReconcileOutcome::Empty);
    }

    match mode {
        ReconcileMode::Patch => write_patch(worktree, task_id).await,
        ReconcileMode::Branch => commit_to_branch(repo_root, worktree, baseline, task_id).await,
    }
}

async fn write_patch(worktree: &Path, task_id: &str) -> Result<ReconcileOutcome, ReconcileError> {
    let dir = patches_root();
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{task_id}.patch"));
    // Diff against HEAD (parent's pinned ref) — captures the net delta
    // including staged-only changes (we just `git add -A`'d).
    let patch = git::run(
        worktree,
        &["diff", "--cached", "--binary", "HEAD"],
        "diff HEAD",
    )
    .await?;
    tokio::fs::write(&path, &patch).await?;
    Ok(ReconcileOutcome::Patch { path })
}

async fn commit_to_branch(
    repo_root: &Path,
    worktree: &Path,
    baseline: &WorktreeBaseline,
    task_id: &str,
) -> Result<ReconcileOutcome, ReconcileError> {
    // Hooks disabled per RFD's "Out of scope" note.
    let msg = format!("pi/task/{task_id}");
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=pi-worktree",
            "-c",
            "user.email=pi-worktree@example.invalid",
            "commit",
            "--allow-empty",
            "--no-verify",
            "-m",
            &msg,
        ])
        .output()
        .await
        .map_err(|e| ReconcileError::Git(GitError::Io(e)))?;
    if !out.status.success() {
        return Err(ReconcileError::Git(GitError::cmd_failed("commit", &out)));
    }

    let sha_raw = git::run(worktree, &["rev-parse", "HEAD"], "rev-parse").await?;
    let sha = String::from_utf8_lossy(&sha_raw).trim().to_string();

    let branch = format!("pi/task/{task_id}");
    // Replace any pre-existing branch with the same name (idempotent).
    let _ = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["branch", "-D", &branch])
        .output()
        .await;
    git::run(repo_root, &["branch", &branch, &sha], "branch").await?;

    // Best-effort cherry-pick if HEAD hasn't moved.
    let parent_head = git::head_sha(repo_root).await?;
    if !parent_head.is_empty() && parent_head == baseline.head_sha() {
        let out = tokio::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=pi-worktree",
                "-c",
                "user.email=pi-worktree@example.invalid",
                "cherry-pick",
                "--allow-empty",
                &sha,
            ])
            .output()
            .await
            .map_err(|e| ReconcileError::Git(GitError::Io(e)))?;
        if out.status.success() {
            return Ok(ReconcileOutcome::Branch {
                branch,
                sha,
                merged: true,
            });
        }
        // Conflict — abort so the parent worktree is clean again.
        let _ = tokio::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["cherry-pick", "--abort"])
            .output()
            .await;
        return Ok(ReconcileOutcome::ConflictedBranch { branch });
    }

    // HEAD moved — never mutate parent. Branch is preserved for
    // manual resolution (per RFD's hard rule).
    Ok(ReconcileOutcome::ConflictedBranch { branch })
}
