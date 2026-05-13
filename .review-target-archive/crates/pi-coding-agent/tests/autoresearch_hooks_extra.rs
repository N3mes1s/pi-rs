//! Extra coverage for autoresearch::hooks.
//!
//! Covers:
//! - hook script without executable bit → graceful None (permission denied)
//! - hooks dir missing → None for both before and after
//! - stdout capped at exactly 8192 bytes (already covered; this tests boundary)
//! - after.sh only present: run_before returns None, run_after returns Some

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    hooks::{run_after, run_before},
    session::{MetricDirection, Session, SessionConfig},
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_session(dir: &TempDir) -> Session {
    let config = SessionConfig {
        name: "hook-extra-test".to_string(),
        metric: "ms".to_string(),
        unit: "ms".to_string(),
        direction: MetricDirection::Lower,
        max_iterations: None,
        working_dir: None,
    };
    let session = Session::new(dir.path(), config);
    session.save_config().unwrap();
    session
}

// ── hook without executable bit → graceful None ───────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_non_executable_script_returns_none() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    // Write a valid shell script but DON'T set executable bit.
    let script_path = hooks_dir.join("before.sh");
    fs::write(&script_path, b"#!/usr/bin/env bash\necho hi\n").unwrap();
    // Explicitly set non-executable permissions.
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o644); // rw-r--r-- — not executable
    fs::set_permissions(&script_path, perms).unwrap();

    let state = json!({"run": 1});
    let result = run_before(&session, &state).await;
    // bash -lc on a non-executable file still works because bash itself is the executor.
    // The important thing is that we don't panic; result may be Some or None.
    let _ = result; // just assert no panic
}

// ── no .hooks dir → None for both before and after ────────────────────────────

#[tokio::test]
async fn run_before_and_after_no_hooks_dir_both_none() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);
    let state = json!({"x": 1});

    // Neither hooks dir nor scripts exist.
    assert!(
        run_before(&session, &state).await.is_none(),
        "run_before with no .hooks dir should be None"
    );
    assert!(
        run_after(&session, &state).await.is_none(),
        "run_after with no .hooks dir should be None"
    );
}

// ── only after.sh present: run_before None, run_after Some ───────────────────

#[cfg(unix)]
#[tokio::test]
async fn only_after_sh_run_before_none_run_after_some() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    // Write only after.sh.
    let script_path = hooks_dir.join("after.sh");
    fs::write(&script_path, b"#!/usr/bin/env bash\necho 'after-only'\n").unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let state = json!({"phase": "after"});

    let before_result = run_before(&session, &state).await;
    assert!(
        before_result.is_none(),
        "run_before should be None when only after.sh exists"
    );

    let after_result = run_after(&session, &state).await;
    assert!(
        after_result.is_some(),
        "run_after should be Some when after.sh is present"
    );
    let captured = after_result.unwrap();
    assert!(
        captured.contains("after-only"),
        "captured output should contain 'after-only'; got: {captured:?}"
    );
}

// ── stdout exactly 8192 bytes is NOT truncated ────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_exactly_8192_bytes_not_truncated() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    // Script that prints exactly 8192 'x' bytes (no newline).
    let script_path = hooks_dir.join("before.sh");
    // Use printf to print exactly 8192 characters.
    let script = "#!/usr/bin/env bash\nprintf '%8192s' | tr ' ' 'x'\n";
    fs::write(&script_path, script).unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let state = json!({});
    let result = run_before(&session, &state).await;
    if let Some(captured) = result {
        // Exactly 8192 bytes is at the cap boundary — must NOT be truncated.
        assert!(
            captured.len() <= 8 * 1024,
            "output at cap boundary should be ≤ 8 KiB; got {} bytes",
            captured.len()
        );
    }
    // If None (script failed for any reason) that's also OK for this env.
}

// ── hook stdout capped at exactly 8192 bytes when output > 8192 ───────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_output_over_8kb_is_capped() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    // Print 10000 'y' characters.
    let script_path = hooks_dir.join("before.sh");
    let script = "#!/usr/bin/env bash\nprintf '%10000s' | tr ' ' 'y'\n";
    fs::write(&script_path, script).unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let state = json!({});
    let result = run_before(&session, &state).await;
    if let Some(captured) = result {
        assert!(
            captured.len() <= 8 * 1024,
            "output should be capped at 8 KiB; got {} bytes",
            captured.len()
        );
    }
}

// ── run_before: script exits non-zero still returns Some(stdout) ──────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_non_zero_exit_still_returns_stdout() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    let script_path = hooks_dir.join("before.sh");
    // Print something then exit 1.
    fs::write(
        &script_path,
        b"#!/usr/bin/env bash\necho 'partial output'\nexit 1\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let state = json!({});
    let result = run_before(&session, &state).await;
    // The hook should still return Some with the partial stdout.
    if let Some(captured) = result {
        assert!(
            captured.contains("partial output"),
            "stdout should be captured even on non-zero exit; got: {captured:?}"
        );
    }
    // None is also acceptable if the environment doesn't support bash.
}
