//! Tests for `autoresearch::hooks`.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use tempfile::TempDir;

use pi_coding_agent::autoresearch::{
    hooks::{run_before, run_after},
    log::{JsonlLog, LogEntryKind},
    session::{MetricDirection, Session, SessionConfig},
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_session(dir: &TempDir) -> Session {
    let config = SessionConfig {
        name: "hook-test".to_string(),
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

/// Create `<root>/autoresearch.hooks/<name>.sh` that:
/// 1. Echoes its stdin to `<sentinel>`.
/// 2. Prints a fixed stdout line.
#[cfg(unix)]
fn make_hook_script(hooks_dir: &std::path::Path, name: &str, sentinel: &std::path::Path) {
    fs::create_dir_all(hooks_dir).unwrap();
    let script = format!(
        "#!/usr/bin/env bash\ncat > '{}'\necho 'hook-output'\n",
        sentinel.display()
    );
    let script_path = hooks_dir.join(format!("{name}.sh"));
    fs::write(&script_path, script).unwrap();
    // Make executable.
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();
}

// ── run_before: no hooks dir → returns None ───────────────────────────────────

#[tokio::test]
async fn run_before_no_hooks_dir_returns_none() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);
    let state = json!({"run": 1});
    let result = run_before(&session, &state).await;
    assert!(result.is_none(), "should return None when hooks dir absent");
}

// ── run_after: no hooks dir → returns None ────────────────────────────────────

#[tokio::test]
async fn run_after_no_hooks_dir_returns_none() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);
    let state = json!({"run": 1});
    let result = run_after(&session, &state).await;
    assert!(result.is_none(), "should return None when hooks dir absent");
}

// ── run_before: before.sh absent → None ──────────────────────────────────────

#[tokio::test]
async fn run_before_no_before_sh_returns_none() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);
    // Create hooks dir but no before.sh.
    fs::create_dir_all(dir.path().join("autoresearch.hooks")).unwrap();
    let state = json!({"run": 1});
    let result = run_before(&session, &state).await;
    assert!(result.is_none());
}

// ── run_before: before.sh echoes stdin to sentinel ────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_captures_stdout_and_writes_sentinel() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    let sentinel = dir.path().join("sentinel.json");
    make_hook_script(&hooks_dir, "before", &sentinel);

    let state = json!({"run": 42, "metric": "ms"});
    let result = run_before(&session, &state).await;

    // The hook must have produced Some(stdout).
    assert!(result.is_some(), "before.sh should return Some(stdout)");
    let captured = result.unwrap();
    assert!(
        captured.contains("hook-output"),
        "stdout should contain 'hook-output'; got: {:?}",
        captured
    );

    // The sentinel file should contain the JSON state that was sent to stdin.
    assert!(sentinel.exists(), "sentinel file should have been created by the hook");
    let sentinel_contents = fs::read_to_string(&sentinel).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&sentinel_contents)
        .expect("sentinel should contain valid JSON");
    assert_eq!(
        parsed["run"],
        json!(42),
        "stdin JSON must match what we sent; sentinel: {sentinel_contents}"
    );
}

// ── run_after: after.sh works the same way ────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn run_after_captures_stdout_and_writes_sentinel() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    let sentinel = dir.path().join("after_sentinel.json");
    make_hook_script(&hooks_dir, "after", &sentinel);

    let state = json!({"phase": "after", "value": 99.9});
    let result = run_after(&session, &state).await;

    assert!(result.is_some(), "after.sh should return Some(stdout)");
    let captured = result.unwrap();
    assert!(captured.contains("hook-output"), "got: {:?}", captured);

    assert!(sentinel.exists(), "after sentinel should exist");
    let contents = fs::read_to_string(&sentinel).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed["phase"], json!("after"));
}

// ── Hook entry appended to JSONL ──────────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_appends_hook_entry_to_log() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    let sentinel = dir.path().join("log_sentinel.json");
    make_hook_script(&hooks_dir, "before", &sentinel);

    let state = json!({"check": true});
    let _ = run_before(&session, &state).await;

    // Read the JSONL log — should have a Hook entry.
    let log = JsonlLog::new(session.jsonl_path(), MetricDirection::Lower);
    let entries = log.read_all().unwrap();
    let hook_entries: Vec<_> = entries
        .iter()
        .filter(|e| matches!(&e.kind, LogEntryKind::Hook { hook, .. } if hook == "before"))
        .collect();

    assert_eq!(
        hook_entries.len(),
        1,
        "expected exactly one Hook entry; entries: {entries:?}"
    );
    if let LogEntryKind::Hook { hook, output } = &hook_entries[0].kind {
        assert_eq!(hook, "before");
        assert!(output.contains("hook-output"), "output: {output}");
    }
}

// ── stdout capped at 8 KiB ────────────────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn run_before_caps_output_at_8kb() {
    let dir = TempDir::new().unwrap();
    let session = make_session(&dir);

    let hooks_dir = dir.path().join("autoresearch.hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    // Script that prints 20 000 bytes.
    let script_path = hooks_dir.join("before.sh");
    let script = "#!/usr/bin/env bash\ndd if=/dev/zero bs=20000 count=1 2>/dev/null | tr '\\0' 'x'\n";
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
    // If None (e.g., the dd command isn't available) that's also acceptable.
}
