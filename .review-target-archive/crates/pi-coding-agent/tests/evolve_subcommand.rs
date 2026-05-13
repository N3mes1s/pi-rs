//! Smoke tests for `pi --evolve {status,off,on}` (G10).
//!
//! Spawns the real binary with the appropriate flags in a tempdir cwd
//! and asserts the side effects (disabled flag created/removed, status
//! summary printed).

use std::process::Command;

fn pi_binary() -> std::path::PathBuf {
    // The integration tests run after the workspace builds, so we know
    // the dev pi binary exists at this canonical location.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

#[test]
fn evolve_off_creates_disabled_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(pi_binary())
        .args(["--evolve", "off"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.path().join(".pi/evolve/disabled").exists());
    assert!(String::from_utf8_lossy(&out.stdout).contains("evolve: disabled"));
}

#[test]
fn evolve_on_removes_disabled_flag() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-populate.
    let flag = dir.path().join(".pi/evolve/disabled");
    std::fs::create_dir_all(flag.parent().unwrap()).unwrap();
    std::fs::write(&flag, "").unwrap();

    let out = Command::new(pi_binary())
        .args(["--evolve", "on"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!flag.exists(), "disabled flag should be removed");
    assert!(String::from_utf8_lossy(&out.stdout).contains("evolve: enabled"));
}

#[test]
fn evolve_status_in_fresh_cwd_prints_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(pi_binary())
        .args(["--evolve", "status"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("evolve status for"));
    assert!(stdout.contains("ticks_run:      0"));
    assert!(stdout.contains("enabled-here:   yes"));
    assert!(stdout.contains("$0.0000"));
}

#[test]
fn evolve_status_reports_disabled_state() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join(".pi/evolve/disabled");
    std::fs::create_dir_all(flag.parent().unwrap()).unwrap();
    std::fs::write(&flag, "").unwrap();

    let out = Command::new(pi_binary())
        .args(["--evolve", "status"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("enabled-here:   no"));
}

#[test]
fn evolve_with_unknown_verb_rejected_by_clap() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(pi_binary())
        .args(["--evolve", "destroy"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    // clap rejects values not in the PossibleValuesParser list before
    // we even reach run_evolve — exit code 2.
    assert!(!out.status.success());
}

#[test]
fn evolve_dry_run_in_empty_cwd_reports_no_agents_md_and_exits_zero() {
    // No AGENTS.md, no past trajectories. Dry-run should still succeed
    // — it's a preview, not a hard failure — and surface the missing
    // file so the user understands why the daemon would skip.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(pi_binary())
        .args(["--evolve", "dry-run"])
        .current_dir(dir.path())
        .env("HOME", dir.path()) // isolate global AGENTS.md fallback
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("evolve --dry-run for"));
    assert!(stdout.contains("has_agents_md:    false"));
    assert!(stdout.contains("would SKIP"));
    // Confirms we never reached the prompt-rendering branch.
    assert!(!stdout.contains("sample mutator prompt"));
}

#[test]
fn evolve_dry_run_with_agents_md_renders_sample_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // Minimal AGENTS.md with one mutable section.
    std::fs::write(
        dir.path().join("AGENTS.md"),
        "# AGENTS\n\n## Tools\n\nPrefer `rg` over `grep`.\n",
    )
    .unwrap();
    let out = Command::new(pi_binary())
        .args(["--evolve", "dry-run"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("has_agents_md:    true"));
    assert!(stdout.contains("Mutable sections"));
    assert!(stdout.contains("Next mutation target"));
    assert!(stdout.contains("sample mutator prompt"));
    // build_prompt always emits the heading + current_body XML wrappers.
    assert!(stdout.contains("<heading>"));
    assert!(stdout.contains("<current_body>"));
    // We never reach the slow model.
    assert!(stdout.contains("no model call made"));
    assert!(stdout.contains("AGENTS.md untouched"));
}
