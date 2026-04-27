//! Lifecycle hook execution for autoresearch.
//!
//! Walks `<root>/autoresearch.hooks/` for `before.sh` / `after.sh`, runs each
//! via `bash -lc`, writes the JSON run-state to the hook's stdin, captures
//! stdout (capped at 8 KiB), appends a [`LogEntryKind::Hook`] entry to the
//! JSONL log, and returns the captured stdout (intended as a steer message for
//! the agent).
//!
//! Both functions are no-ops when the hooks directory or the specific script
//! does not exist.

use std::io::Write;
use std::process::Stdio;

use crate::autoresearch::session::Session;

const MAX_OUTPUT_BYTES: usize = 8 * 1024; // 8 KiB cap on hook stdout

// ── public API ────────────────────────────────────────────────────────────────

/// Run `<root>/autoresearch.hooks/before.sh` (if it exists).
///
/// Writes `run_state` as JSON to the hook's stdin, captures stdout (≤ 8 KiB),
/// appends a `Hook { hook: "before", output }` entry to the JSONL log, and
/// returns the captured output.
pub async fn run_before(
    session: &Session,
    run_state: &serde_json::Value,
) -> Option<String> {
    run_hook(session, "before", run_state).await
}

/// Run `<root>/autoresearch.hooks/after.sh` (if it exists).
///
/// Same semantics as [`run_before`] but for the `after` hook.
pub async fn run_after(
    session: &Session,
    run_state: &serde_json::Value,
) -> Option<String> {
    run_hook(session, "after", run_state).await
}

// ── implementation ────────────────────────────────────────────────────────────

async fn run_hook(
    session: &Session,
    hook_name: &str,
    run_state: &serde_json::Value,
) -> Option<String> {
    let script_path = session
        .root
        .join("autoresearch.hooks")
        .join(format!("{}.sh", hook_name));

    if !script_path.exists() {
        return None;
    }

    // Serialise run_state for stdin.
    let stdin_json = serde_json::to_string(run_state).unwrap_or_else(|_| "{}".into());

    // Spawn `bash -lc <path>` with the script path as the argument.
    let script_str = script_path.display().to_string();

    let mut child = match std::process::Command::new("bash")
        .args(["-lc", &script_str])
        .current_dir(&session.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return None,
    };

    // Write JSON state to stdin, then close stdin so the script can read EOF.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_json.as_bytes());
        // stdin is closed when dropped here.
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return None,
    };

    // Cap to 8 KiB.
    let raw = String::from_utf8_lossy(&output.stdout);
    let captured = if raw.len() > MAX_OUTPUT_BYTES {
        raw[..MAX_OUTPUT_BYTES].to_string()
    } else {
        raw.into_owned()
    };

    // Hooks don't appear in the upstream JSONL schema as their own entry
    // type — the agent receives the captured stdout as a steer message and
    // can record what it learned via `log_experiment`'s `asi` field. We
    // intentionally don't write a hook entry here.
    let _ = session;
    let _ = hook_name;
    Some(captured)
}
