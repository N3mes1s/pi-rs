//! Capture / re-apply parent repo state inside the worktree so the
//! subagent sees the user's WIP.

use super::git::{self, GitError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Hard cap on individual untracked file size during baseline capture.
pub const WORKTREE_MAX_UNTRACKED_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("git error: {0}")]
    Git(#[from] GitError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrackedFile {
    pub rel_path: PathBuf,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoBaseline {
    pub repo_root: PathBuf,
    pub head_sha: String,
    pub staged: Vec<u8>,
    pub unstaged: Vec<u8>,
    pub untracked: Vec<UntrackedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeBaseline {
    pub root: RepoBaseline,
    pub nested: Vec<(PathBuf, RepoBaseline)>,
}

impl WorktreeBaseline {
    pub fn head_sha(&self) -> &str {
        &self.root.head_sha
    }
}

pub async fn capture(repo_root: &Path) -> Result<WorktreeBaseline, BaselineError> {
    let root = capture_repo(repo_root).await?;
    Ok(WorktreeBaseline {
        root,
        nested: Vec::new(),
    })
}

async fn capture_repo(repo_root: &Path) -> Result<RepoBaseline, BaselineError> {
    let head_sha = git::head_sha(repo_root).await?;
    let staged = git::run(
        repo_root,
        &["diff", "--cached", "--binary"],
        "diff --cached",
    )
    .await?;
    let unstaged = git::run(repo_root, &["diff", "--binary"], "diff").await?;

    let untracked_list = git::run(
        repo_root,
        &["ls-files", "-o", "--exclude-standard", "-z"],
        "ls-files -o",
    )
    .await?;

    let mut untracked = Vec::new();
    for entry in untracked_list.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let rel = match std::str::from_utf8(entry) {
            Ok(s) => PathBuf::from(s),
            Err(_) => continue,
        };
        let abs = repo_root.join(&rel);
        let meta = match tokio::fs::metadata(&abs).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > WORKTREE_MAX_UNTRACKED_BYTES {
            tracing::warn!(
                ?rel,
                size = meta.len(),
                "skipping untracked file > 16 MiB during worktree baseline capture"
            );
            continue;
        }
        let content = tokio::fs::read(&abs).await?;
        untracked.push(UntrackedFile {
            rel_path: rel,
            content,
        });
    }

    Ok(RepoBaseline {
        repo_root: repo_root.to_path_buf(),
        head_sha,
        staged,
        unstaged,
        untracked,
    })
}

pub async fn apply(
    target_worktree: &Path,
    baseline: &WorktreeBaseline,
) -> Result<(), BaselineError> {
    apply_repo(target_worktree, &baseline.root).await?;
    for (rel, sub) in &baseline.nested {
        let target = target_worktree.join(rel);
        apply_repo(&target, sub).await?;
    }
    Ok(())
}

async fn apply_repo(target: &Path, base: &RepoBaseline) -> Result<(), BaselineError> {
    if !base.staged.is_empty() {
        git::run_stdin(
            target,
            &["apply", "--binary", "--cached"],
            &base.staged,
            "apply --cached",
        )
        .await?;
    }
    if !base.unstaged.is_empty() {
        git::run_stdin(target, &["apply", "--binary"], &base.unstaged, "apply").await?;
    }
    for f in &base.untracked {
        let dst = target.join(&f.rel_path);
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&dst, &f.content).await?;
    }
    Ok(())
}
