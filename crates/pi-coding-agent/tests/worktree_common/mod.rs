//! Shared helpers for the worktree integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn require_git() -> bool {
    if which::which("git").is_err() {
        eprintln!("git not on PATH; skipping worktree integration test");
        return false;
    }
    true
}

pub fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git");
    if !out.status.success() {
        panic!(
            "git -C {} {:?} failed: {}\n{}",
            repo.display(),
            args,
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
    }
    out
}

/// `git init` a fresh repo at `dir`, configure identity, drop a seed
/// commit, return the repo root.
pub fn init_seed_repo(dir: &Path) -> PathBuf {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q", "-b", "main"])
        .output()
        .expect("git init");
    git(dir, &["config", "user.name", "pi-test"]);
    git(dir, &["config", "user.email", "pi@test.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "seed", "--no-verify"]);
    dir.to_path_buf()
}

/// Set `PI_WORKTREE_ROOT` to a tempdir-scoped path so tests don't
/// pollute `~/.pi/wt/`. Returns the override path; keep it alive.
pub fn isolate_wt_root(tmp: &Path) -> PathBuf {
    let p = tmp.join("pi-wt-root");
    std::env::set_var("PI_WORKTREE_ROOT", &p);
    p
}
