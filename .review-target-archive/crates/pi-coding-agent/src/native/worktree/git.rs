//! Tiny async wrappers around the `git` CLI. We shell out (no libgit2)
//! to keep the dependency footprint flat and to mirror the existing
//! pattern in `crate::footer`.

use std::path::Path;
use std::process::Output;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git {op} failed (status={status:?}): {stderr}")]
    CmdFailed {
        op: String,
        status: Option<i32>,
        stderr: String,
        stdout: String,
    },
}

impl GitError {
    pub fn cmd_failed(op: &str, out: &Output) -> Self {
        GitError::CmdFailed {
            op: op.to_string(),
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        }
    }
}

fn cmd() -> Command {
    Command::new("git")
}

pub async fn run(repo: &Path, args: &[&str], op: &str) -> Result<Vec<u8>, GitError> {
    let out = cmd().arg("-C").arg(repo).args(args).output().await?;
    if !out.status.success() {
        return Err(GitError::cmd_failed(op, &out));
    }
    Ok(out.stdout)
}

pub async fn run_stdin(
    repo: &Path,
    args: &[&str],
    stdin: &[u8],
    op: &str,
) -> Result<Vec<u8>, GitError> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut child = cmd()
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut s) = child.stdin.take() {
        s.write_all(stdin).await?;
        s.shutdown().await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        return Err(GitError::cmd_failed(op, &out));
    }
    Ok(out.stdout)
}

pub async fn worktree_add_detached(
    repo_root: &Path,
    path: &Path,
    refspec: &str,
) -> Result<(), GitError> {
    let out = cmd()
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "add", "--detach", "--no-checkout"])
        .arg(path)
        .arg(refspec)
        .output()
        .await?;
    if !out.status.success() {
        return Err(GitError::cmd_failed("worktree add", &out));
    }
    let out = cmd()
        .arg("-C")
        .arg(path)
        .args(["checkout", "--detach", refspec])
        .output()
        .await?;
    if !out.status.success() {
        return Err(GitError::cmd_failed("checkout --detach", &out));
    }
    Ok(())
}

pub async fn worktree_try_remove(repo_root: &Path, path: &Path) -> Result<(), GitError> {
    let out = cmd()
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(path)
        .output()
        .await?;
    if !out.status.success() {
        return Err(GitError::cmd_failed("worktree remove", &out));
    }
    Ok(())
}

/// `git -C <p> rev-parse --show-toplevel` — find the parent repo of a
/// worktree (or any inside-tree dir).
pub async fn repo_root(p: &Path) -> Result<std::path::PathBuf, GitError> {
    let out = run(p, &["rev-parse", "--show-toplevel"], "rev-parse").await?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    Ok(std::path::PathBuf::from(s))
}

pub async fn head_sha(repo: &Path) -> Result<String, GitError> {
    match run(repo, &["rev-parse", "HEAD"], "rev-parse HEAD").await {
        Ok(out) => Ok(String::from_utf8_lossy(&out).trim().to_string()),
        // Orphan / unborn HEAD ⇒ empty string per RFD.
        Err(GitError::CmdFailed { .. }) => Ok(String::new()),
        Err(e) => Err(e),
    }
}
