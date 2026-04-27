//! Tests for `extensions::run_startup_hooks`.
//!
//! Scenarios:
//!   1. An extension whose `startup_executable` writes a sentinel file — we
//!      assert the file exists after `run_startup_hooks` returns.
//!   2. An extension whose `startup_executable` points at a missing binary —
//!      `run_startup_hooks` must log a warning and return without panicking.
//!   3. An extension with `startup_executable: None` — no subprocess is
//!      spawned.

use pi_coding_agent::extensions::{run_startup_hooks, ExtensionManifest, LoadedExtension};
use std::os::unix::fs::PermissionsExt;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Build a `LoadedExtension` with the given `startup_executable` path (or
/// `None`), pointing at `/bin/true` as the regular extension executable.
fn ext_with_startup(
    root: &std::path::Path,
    startup_executable: Option<String>,
) -> LoadedExtension {
    LoadedExtension {
        manifest: ExtensionManifest {
            name: "startup-test-ext".into(),
            version: "0.1.0".into(),
            executable: "/bin/true".into(),
            tools: vec![],
            commands: vec![],
            timeout_ms: Some(5_000),
            keybindings: vec![],
            hooks: vec![],
            replaces_builtin: vec![],
            startup_executable,
        },
        root: root.to_path_buf(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// A `startup_executable` that creates a sentinel file must actually create it.
#[tokio::test]
async fn startup_hook_runs_and_creates_sentinel() {
    let ext_dir = tempfile::tempdir().unwrap();
    let sentinel_dir = tempfile::tempdir().unwrap();
    let sentinel = sentinel_dir.path().join("started.txt");

    // Shell script: create the sentinel file.
    let script_body = format!(
        "#!/bin/sh\ntouch \"{}\"\n",
        sentinel.display()
    );
    let script_path = ext_dir.path().join("startup.sh");
    make_executable(&script_path, &script_body);

    let ext = ext_with_startup(ext_dir.path(), Some("./startup.sh".to_string()));
    run_startup_hooks(&[ext]).await;

    assert!(
        sentinel.exists(),
        "sentinel file should have been created by startup_executable"
    );
}

/// A missing `startup_executable` must log a warning and not panic.
#[tokio::test]
async fn startup_hook_missing_executable_does_not_panic() {
    let ext_dir = tempfile::tempdir().unwrap();

    let ext = ext_with_startup(
        ext_dir.path(),
        Some("./does-not-exist-startup.sh".to_string()),
    );

    // Must not panic.
    run_startup_hooks(&[ext]).await;
}

/// An extension with `startup_executable: None` must be silently skipped.
#[tokio::test]
async fn startup_hook_none_startup_executable_is_skipped() {
    let ext_dir = tempfile::tempdir().unwrap();
    // No sentinel to check; we just confirm there's no panic.
    let ext = ext_with_startup(ext_dir.path(), None);
    run_startup_hooks(&[ext]).await;
}

/// When there are no extensions at all, `run_startup_hooks` must return
/// immediately without error.
#[tokio::test]
async fn startup_hook_empty_slice_is_noop() {
    run_startup_hooks(&[]).await;
}

/// Multiple extensions: one with a valid startup hook, one without. The valid
/// hook runs; the `None` one is skipped silently.
#[tokio::test]
async fn startup_hook_mixed_extensions() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let sentinel_dir = tempfile::tempdir().unwrap();
    let sentinel = sentinel_dir.path().join("ran.txt");

    let script_body = format!("#!/bin/sh\ntouch \"{}\"\n", sentinel.display());
    let script_path = dir_a.path().join("start.sh");
    make_executable(&script_path, &script_body);

    let ext_a = ext_with_startup(dir_a.path(), Some("./start.sh".to_string()));
    let ext_b = ext_with_startup(dir_b.path(), None);

    run_startup_hooks(&[ext_a, ext_b]).await;

    assert!(sentinel.exists(), "sentinel from ext_a startup hook must exist");
}
