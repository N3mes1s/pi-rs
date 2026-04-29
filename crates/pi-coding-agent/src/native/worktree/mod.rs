//! Worktree-isolated tasks (RFD 0006).
//!
//! Allocate a private `git worktree` per task under
//! `~/.pi/wt/data/<encoded-repo>/<task-id>/`, replay the parent's WIP
//! into it via [`baseline::capture`] + [`baseline::apply`], let the
//! caller mutate files there, then fold the result back through
//! [`reconcile::finish`] as either a `pi/task/<id>` branch commit or a
//! `~/.pi/wt/patches/<id>.patch` artifact.

pub mod baseline;
pub mod git;
pub mod reconcile;

use std::path::{Path, PathBuf};
use thiserror::Error;

pub use baseline::{
    apply as apply_baseline, capture as capture_baseline, BaselineError, RepoBaseline,
    UntrackedFile, WorktreeBaseline, WORKTREE_MAX_UNTRACKED_BYTES,
};
pub use reconcile::{finish, ReconcileError, ReconcileMode, ReconcileOutcome};

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("git error: {0}")]
    Git(#[from] git::GitError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Root for all worktree state. Honours `PI_WORKTREE_ROOT` for tests;
/// otherwise lives under [`crate::context::agent_dir`]'s sibling
/// `~/.pi/wt`.
pub fn wt_root() -> PathBuf {
    if let Ok(p) = std::env::var("PI_WORKTREE_ROOT") {
        return PathBuf::from(p);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("wt")
}

pub fn worktrees_root() -> PathBuf {
    wt_root().join("data")
}

pub fn patches_root() -> PathBuf {
    wt_root().join("patches")
}

pub fn encode_repo(repo_root: &Path) -> String {
    let s = repo_root
        .to_string_lossy()
        .replace(['/', ':', '\\'], "-");
    format!("--{}--", s.trim_matches('-'))
}

pub fn worktree_dir(repo_root: &Path, task_id: &str) -> PathBuf {
    worktrees_root().join(encode_repo(repo_root)).join(task_id)
}

pub async fn ensure(repo_root: &Path, task_id: &str) -> Result<PathBuf, WorktreeError> {
    let dir = worktree_dir(repo_root, task_id);
    if let Some(parent) = dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Best-effort cleanup of any stale registration.
    let _ = git::worktree_try_remove(repo_root, &dir).await;
    if dir.exists() {
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
    git::worktree_add_detached(repo_root, &dir, "HEAD").await?;
    Ok(dir)
}

/// Belt-and-braces cleanup. Best-effort: errors are swallowed.
pub async fn cleanup(dir: &Path) {
    if let Ok(repo) = git::repo_root(dir).await {
        let _ = git::worktree_try_remove(&repo, dir).await;
    }
    let _ = tokio::fs::remove_dir_all(dir).await;
}

/// RAII guard: cleans up the worktree on drop. Spawns a blocking task
/// to run async cleanup synchronously since [`Drop`] can't await.
pub struct WorktreeGuard {
    dir: Option<PathBuf>,
}

impl WorktreeGuard {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }
    pub fn path(&self) -> &Path {
        self.dir.as_deref().expect("guard already disarmed")
    }
    pub fn disarm(mut self) -> PathBuf {
        self.dir.take().expect("guard already disarmed")
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            // Use a blocking std::process::Command for the git remove;
            // we can't reliably reach a tokio runtime from Drop.
            if let Ok(out) = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["rev-parse", "--show-toplevel"])
                .output()
            {
                if out.status.success() {
                    if let Ok(repo) = std::str::from_utf8(&out.stdout) {
                        let repo = repo.trim();
                        let _ = std::process::Command::new("git")
                            .arg("-C")
                            .arg(repo)
                            .args(["worktree", "remove", "--force"])
                            .arg(&dir)
                            .output();
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
