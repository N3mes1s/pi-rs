//! Git status helper used by the powerline footer.
//!
//! `git status --porcelain=v1` is shelled out behind a 2-second cache
//! so a steady-state UI render doesn't fork a subprocess every frame.
//! Branch name is read with `git symbolic-ref --short HEAD` (falling
//! back to a short SHA via `rev-parse --short HEAD` for detached
//! HEAD).
//!
//! All operations are *best-effort*: any failure (no `git` binary, not
//! a repo, permission error) yields `None`, which the renderer treats
//! as "skip the git segment".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Raw git status: branch + tallies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: String,
    /// Number of *staged* files (anything with a non-space, non-`?` in
    /// the first column of `git status --porcelain=v1`).
    pub staged: u32,
    /// Number of *modified-but-unstaged* files (non-space in column
    /// two, or untracked `??` lines).
    pub modified: u32,
}

/// Mutex-wrapped 2-second cache so the renderer can call
/// [`GitStatusCache::get`] every frame without hammering `git`.
pub struct GitStatusCache {
    inner: Mutex<Option<CacheEntry>>,
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    cwd: PathBuf,
    captured_at: Instant,
    value: Option<GitStatus>,
}

impl Default for GitStatusCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}

impl GitStatusCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(None),
            ttl,
        }
    }

    /// Fetch the cached value for `cwd` or recompute if the entry is
    /// missing, the ttl elapsed, or the cwd differs from what we last
    /// captured.
    pub fn get(&self, cwd: &Path) -> Option<GitStatus> {
        let mut g = self.inner.lock().ok()?;
        let now = Instant::now();
        let stale = match &*g {
            Some(e) if e.cwd == cwd && now.duration_since(e.captured_at) < self.ttl => false,
            _ => true,
        };
        if stale {
            let v = compute_status(cwd);
            *g = Some(CacheEntry {
                cwd: cwd.to_path_buf(),
                captured_at: now,
                value: v.clone(),
            });
            v
        } else {
            g.as_ref().and_then(|e| e.value.clone())
        }
    }

    /// Test helper: drop any cached value so the next call recomputes.
    pub fn invalidate(&self) {
        if let Ok(mut g) = self.inner.lock() {
            *g = None;
        }
    }
}

fn compute_status(cwd: &Path) -> Option<GitStatus> {
    let branch = git_branch(cwd)?;
    let porcelain = run_git(cwd, &["status", "--porcelain=v1"])?;
    let mut staged = 0u32;
    let mut modified = 0u32;
    for line in porcelain.lines() {
        let mut chars = line.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        if x == '?' && y == '?' {
            modified += 1;
        } else {
            if x != ' ' && x != '?' {
                staged += 1;
            }
            if y != ' ' && y != '?' {
                modified += 1;
            }
        }
    }
    Some(GitStatus {
        branch,
        staged,
        modified,
    })
}

fn git_branch(cwd: &Path) -> Option<String> {
    if let Some(name) = run_git(cwd, &["symbolic-ref", "--short", "HEAD"]) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    // Detached HEAD: fall back to short SHA.
    run_git(cwd, &["rev-parse", "--short", "HEAD"]).map(|s| s.trim().to_string())
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").current_dir(cwd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Render the git segment "branch ●N+M". `staged + modified == 0`
/// renders just the branch.
pub fn format_git(status: &GitStatus) -> String {
    let dirty = status.staged + status.modified;
    if dirty == 0 {
        format!("git: {}", status.branch)
    } else {
        format!("git: {} ●{}+{}", status.branch, status.staged, status.modified)
    }
}
