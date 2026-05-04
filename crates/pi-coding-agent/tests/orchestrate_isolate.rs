//! Integration tests for `--orchestrate-isolate` (RFD 0021 M2 extension).
//!
//! Tests verify that:
//!   1. A fresh git worktree is allocated when `--orchestrate-isolate` is used.
//!   2. The worktree is removed after the run (no leftover `pi-orch-*` dir).
//!   3. The campaign's MERGED commit is visible on `main` in the parent repo.
//!
//! We exercise the worktree-allocation contract directly (rather than spawning
//! a real `pi` subprocess that would require live LLM credentials) by
//! reproducing the exact git commands that `pi.rs` runs and feeding the runner
//! a `FakeDispatch`. This is the same pattern used in
//! `crates/pi-orchestrate/tests/runner_v1.rs`.

use pi_orchestrate::dispatch::{Dispatch, DispatchOutcome, DispatchRole};
use pi_orchestrate::{parse_campaign, replay, run_with, validate};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use tempfile::tempdir;

// ─── shared fake dispatcher ──────────────────────────────────────────────────

struct FakeDispatch {
    canned: Mutex<Vec<DispatchOutcome>>,
}

impl FakeDispatch {
    fn new(canned: Vec<DispatchOutcome>) -> Self {
        Self { canned: Mutex::new(canned) }
    }
}

impl Dispatch for FakeDispatch {
    fn dispatch(
        &self,
        _role: DispatchRole,
        _agent_name: &str,
        _assignment: &str,
        _cwd: &Path,
    ) -> std::io::Result<DispatchOutcome> {
        let mut q = self.canned.lock().unwrap();
        if q.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "FakeDispatch ran out of canned outcomes",
            ));
        }
        Ok(q.remove(0))
    }
}

fn ok_outcome(text: &str) -> DispatchOutcome {
    DispatchOutcome {
        agent: "fake".into(),
        success: true,
        model_output: text.to_string(),
        stderr: String::new(),
        exit_code: 0,
        duration_ms: 0,
    }
}

// ─── git helpers ─────────────────────────────────────────────────────────────

fn git(p: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(p)
        .output()
        .unwrap();
    if !out.status.success() {
        panic!(
            "git {:?} in {} failed: {}",
            args,
            p.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn git_output(p: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Create a tempdir git repo with `main` branch + one commit +
/// a `feat/m1` branch that adds one file.  The parent working tree
/// is left in **detached-HEAD** mode so that linked worktrees can
/// `git checkout main` without hitting the "branch is already used by
/// another worktree" error.  Returns the TempDir.
fn make_repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.email", "test@example.com"]);
    git(p, &["config", "user.name", "Test"]);
    git(p, &["config", "commit.gpgsign", "false"]);
    git(p, &["config", "tag.gpgsign", "false"]);
    std::fs::write(p.join("README.md"), "base\n").unwrap();
    git(p, &["add", "README.md"]);
    git(p, &["commit", "-q", "-m", "base"]);
    // feat/m1 branch
    git(p, &["checkout", "-q", "-b", "feat/m1"]);
    std::fs::write(p.join("feat.txt"), "milestone 1\n").unwrap();
    git(p, &["add", "feat.txt"]);
    git(p, &["commit", "-q", "-m", "feat: m1"]);
    git(p, &["checkout", "-q", "main"]);
    // Detach the parent HEAD so that linked worktrees (created by
    // `git worktree add --detach`) are free to `git checkout main`
    // during cherry-pick without hitting "already used by worktree".
    git(p, &["checkout", "--detach", "HEAD"]);
    dir
}

const CAMPAIGN_TOML: &str = r#"
name = "isolate-test"
target_branch = "main"

[defaults]
reviewer    = "code-reviewer"
fix_loop_max = 1

[[milestones]]
id          = "m1"
branch      = "feat/m1"
implementer = "halo-implementer"
assignment  = "implement m1"
"#;

// ─── helper: allocate + deallocate a worktree (mirrors pi.rs logic) ──────────

fn worktree_add(repo: &Path, wt_path: &Path) {
    let out = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(wt_path)
        .arg("HEAD")
        .current_dir(repo)
        .output()
        .expect("git worktree add failed to run");
    assert!(
        out.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn worktree_remove(repo: &Path, wt_path: &Path) -> bool {
    Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(wt_path)
        .current_dir(repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ─── tests ───────────────────────────────────────────────────────────────────

/// The happy path: run a one-milestone campaign inside an isolated worktree,
/// verify the worktree is cleaned up and the commit lands on `main`.
#[test]
fn orchestrate_isolate_cleans_up_worktree_and_merges_to_main() {
    let repo = make_repo();
    let repo_path = repo.path();

    // Allocate the worktree just like pi.rs does.
    let safe_name = "isolate-test"; // campaign name, slashes → _
    let uid = uuid::Uuid::new_v4().simple().to_string();
    let wt_path = std::env::temp_dir().join(format!("pi-orch-{}-{}", safe_name, &uid[..8]));

    worktree_add(repo_path, &wt_path);
    assert!(wt_path.exists(), "worktree dir must exist after add");

    // Run the campaign inside the worktree with a fake dispatcher.
    let campaign = {
        let c = parse_campaign(CAMPAIGN_TOML).unwrap();
        validate(&c).unwrap();
        c
    };
    let state_root = tempdir().unwrap();
    let dispatcher = FakeDispatch::new(vec![
        ok_outcome("implemented m1"),
        ok_outcome("looks good\n\nMerge readiness: READY_TO_MERGE"),
    ]);
    let summary = run_with(&campaign, state_root.path(), &dispatcher, &wt_path).unwrap();
    assert_eq!(summary.exit_code, 0, "campaign should exit 0");
    assert_eq!(summary.outcomes.len(), 1);
    assert_eq!(summary.outcomes[0].final_state, "MERGED");

    // Best-effort cleanup (mirrors pi.rs).
    let removed = worktree_remove(repo_path, &wt_path);
    assert!(removed, "worktree remove should succeed");

    // Assertion 1: no leftover pi-orch-* directory in temp_dir.
    assert!(
        !wt_path.exists(),
        "worktree dir {} should not exist after removal",
        wt_path.display()
    );

    // Assertion 2: the MERGED commit is on main in the parent repo.
    let log = git_output(repo_path, &["log", "--oneline", "main"]);
    assert!(
        log.contains("feat: m1"),
        "feat/m1 commit should be on main after merge, log={log:?}"
    );

    // Assertion 3: state.jsonl contains a MERGED event.
    let events = replay(&summary.state_path).unwrap();
    let has_merged = events.iter().any(|e| e.to == "MERGED");
    assert!(has_merged, "state.jsonl should contain a MERGED event");
}

/// The CLI flag is parsed correctly by clap.
#[test]
fn cli_orchestrate_isolate_flag_defaults_to_false() {
    use clap::Parser;
    use pi_coding_agent::cli::Cli;

    let cli = Cli::parse_from(["pi"]);
    assert!(!cli.orchestrate_isolate, "defaults to false");

    let cli = Cli::parse_from(["pi", "--orchestrate-isolate"]);
    assert!(cli.orchestrate_isolate, "set to true when passed");
}

/// A worktree directory with the expected naming convention is detectable
/// in temp_dir before removal and absent after.
#[test]
fn worktree_naming_convention_uses_safe_campaign_name_and_8char_uuid_suffix() {
    let repo = make_repo();
    let repo_path = repo.path();

    // Simulate campaign name with a slash — should be sanitised.
    let raw_name = "my/campaign";
    let safe_name = raw_name.replace('/', "_");
    let uid = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &uid[..8];
    let wt_path = std::env::temp_dir().join(format!("pi-orch-{}-{}", safe_name, suffix));

    // Name must contain "pi-orch-" prefix and not contain the raw slash.
    let name_str = wt_path.file_name().unwrap().to_str().unwrap();
    assert!(name_str.starts_with("pi-orch-"), "prefix check: {name_str}");
    assert!(!name_str.contains('/'), "no slash in name: {name_str}");
    assert_eq!(name_str, format!("pi-orch-my_campaign-{suffix}"));

    // Full lifecycle: add → verify exists → remove → verify gone.
    worktree_add(repo_path, &wt_path);
    assert!(wt_path.exists());
    worktree_remove(repo_path, &wt_path);
    assert!(!wt_path.exists());
}
