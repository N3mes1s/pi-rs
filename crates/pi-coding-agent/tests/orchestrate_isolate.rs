//! Integration tests for `--orchestrate-isolate` (RFD 0021 M2 extension).
//!
//! Tests verify that:
//!   1. A fresh git worktree is allocated when `--orchestrate-isolate` is used.
//!   2. The worktree is removed after the run (no leftover `pi-orch-*` dir).
//!   3. The campaign's MERGED commit is visible on `main` in the parent repo.
//!
//! Two levels of coverage:
//!   a. Library-level: exercises `run_with` via a `FakeDispatch` — verifies
//!      merge.rs handles the detached-worktree checkout path correctly.
//!   b. CLI-level: spawns the real `pi` binary with `--orchestrate-isolate`
//!      and verifies worktree cleanup even when the campaign fails (no LLM
//!      credentials needed; we just need the flag to be wired end-to-end).

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
/// stays on `main` (normal checkout, NOT detached) — the merge.rs fix
/// handles the "already used by worktree" error that linked worktrees
/// hit when they try to `git checkout main` while the parent has it.
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
    // Return to main — parent stays on main (realistic scenario).
    git(p, &["checkout", "-q", "main"]);
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

/// Library-level happy path: run a one-milestone campaign inside an isolated
/// worktree (parent stays on `main`), verify the worktree is cleaned up and
/// the commit lands on `main`.
///
/// This specifically validates the merge.rs fix: the linked worktree is
/// detached; when cherry_pick_to_target tries `git checkout main` and gets
/// "already used by worktree", it must fall back to detached-HEAD mode and
/// advance the branch ref via `git branch -f`.
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
    // (The cherry_pick_to_target detach-mode fix must advance the branch ref.)
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

/// CLI-level test: spawn the real `pi` binary with `--orchestrate-isolate`.
///
/// The campaign references agents that don't exist on disk, so `RealDispatch`
/// will return a FAILED outcome quickly (no LLM credentials needed). The key
/// assertions are:
///   1. `pi` exits without panicking (exit code 2 or similar, not a crash).
///   2. No `pi-orch-*` directory survives in `std::env::temp_dir()` after the
///      run — the cleanup path in `src/bin/pi.rs` runs even on campaign failure.
///
/// This exercises the `--orchestrate-isolate` wiring in `src/bin/pi.rs`
/// end-to-end: flag parsing → worktree add → run_with → worktree remove.
#[test]
fn cli_orchestrate_isolate_cleans_up_worktree_on_failure() {
    let repo = make_repo();
    let repo_path = repo.path();

    // Use a unique campaign name to avoid collisions with pi-orch-* directories
    // created by other tests running concurrently in the same test binary.
    let unique_suffix = {
        let uid = uuid::Uuid::new_v4().simple().to_string();
        uid[..8].to_string()
    };
    let campaign_name = format!("cli-isolate-test-{unique_suffix}");
    let safe_campaign_name = campaign_name.replace('/', "_");

    // Write a campaign TOML with the unique name.
    let campaign_toml = format!(
        r#"
name = "{campaign_name}"
target_branch = "main"

[defaults]
reviewer    = "code-reviewer"
fix_loop_max = 1

[[milestones]]
id          = "m1"
branch      = "feat/m1"
implementer = "halo-implementer"
assignment  = "implement m1"
"#
    );
    let toml_path = repo_path.join("campaign.toml");
    std::fs::write(&toml_path, &campaign_toml).unwrap();

    let state_root = tempdir().unwrap();

    // The expected pi-orch-* prefix for this campaign.
    let expected_prefix = format!("pi-orch-{safe_campaign_name}-");

    // Spawn `pi --orchestrate campaign.toml --orchestrate-isolate`.
    let out = Command::new(env!("CARGO_BIN_EXE_pi"))
        .args([
            "--orchestrate",
            toml_path.to_str().unwrap(),
            "--orchestrate-isolate",
            "--orchestrate-state-root",
            state_root.path().to_str().unwrap(),
        ])
        .current_dir(repo_path)
        // Prevent pi from reading real API keys that might trigger LLM calls;
        // the campaign will still fail quickly when agent files are missing.
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("PI_PROVIDER")
        .output()
        .expect("pi binary should spawn");

    // The campaign has no agent .md files → RealDispatch returns FAILED;
    // we expect exit code 2 (at least one FAILED milestone). The binary
    // must NOT exit 0 but also must NOT crash (signal / panic → code 101).
    let exit_code = out.status.code().unwrap_or(-1);
    assert_ne!(
        exit_code, 0,
        "campaign with missing agents should not exit 0"
    );
    // 101 would be an unwrap/panic; -1 means killed by signal.
    assert!(
        exit_code > 0 && exit_code < 100,
        "expected a normal non-zero exit (got {exit_code}); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Assert cleanup: no pi-orch-<campaign_name>-* directory should remain.
    // We filter by the specific expected prefix for this campaign to avoid
    // interference from other tests running in parallel.
    let tmpdir = std::env::temp_dir();
    let leaked: Vec<String> = std::fs::read_dir(&tmpdir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&expected_prefix))
        .collect();

    assert!(
        leaked.is_empty(),
        "pi-orch-* worktree dir(s) for this campaign leaked after --orchestrate-isolate: {:?}",
        leaked
    );
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
