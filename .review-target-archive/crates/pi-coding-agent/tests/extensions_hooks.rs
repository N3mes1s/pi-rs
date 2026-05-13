//! Integration tests for [`HookDispatcher`].
//!
//! Three scenarios:
//!   1. A hook registered for "tool_call" receives the JSON payload via stdin
//!      and writes it to a sentinel file — we assert the file exists and contains
//!      the expected JSON.
//!   2. Dispatching an event that has no registered hooks is a no-op (no panic,
//!      no spawned processes).
//!   3. A hook whose executable doesn't exist logs a warning but does not crash
//!      the dispatcher.

use pi_coding_agent::extensions::{
    ExtensionHook, ExtensionManifest, HookDispatcher, LoadedExtension,
};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Build a minimal [`ExtensionManifest`] that only has the supplied hooks.
fn manifest_with_hooks(hooks: Vec<ExtensionHook>) -> ExtensionManifest {
    ExtensionManifest {
        name: "test-ext".into(),
        version: "0.1.0".into(),
        executable: "./run.sh".into(),
        tools: vec![],
        commands: vec![],
        timeout_ms: Some(5_000),
        keybindings: vec![],
        hooks,
        replaces_builtin: vec![],
        startup_executable: None,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// A hook registered for "tool_call" should receive the payload on stdin and
/// write it to a sentinel file; we verify the file exists and contains the JSON.
#[tokio::test]
async fn hook_receives_payload_and_writes_sentinel() {
    let ext_dir = tempfile::tempdir().unwrap();
    let sentinel_dir = tempfile::tempdir().unwrap();
    let sentinel_path = sentinel_dir.path().join("out.json");

    // Script: read stdin, write it verbatim to the sentinel file.
    let script = format!(
        "#!/bin/sh\nread line\nprintf '%s' \"$line\" > \"{}\"\n",
        sentinel_path.display()
    );
    let hook_exe = ext_dir.path().join("hook.sh");
    make_executable(&hook_exe, &script);

    // Write the manifest (needed so the extension root is valid).
    let manifest = manifest_with_hooks(vec![ExtensionHook {
        event: "tool_call".into(),
        executable: "./hook.sh".into(),
    }]);
    let manifest_path = ext_dir.path().join("pi-extension.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let ext = LoadedExtension {
        manifest,
        root: ext_dir.path().to_path_buf(),
    };

    let dispatcher = HookDispatcher::from_extensions(&[ext]);
    let payload = json!({"name": "bash", "input": {}});
    dispatcher.dispatch("tool_call", &payload).await;

    // The sentinel file must exist and contain the serialised payload.
    assert!(
        sentinel_path.exists(),
        "sentinel file not created by hook script"
    );
    let contents = std::fs::read_to_string(&sentinel_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
    assert_eq!(parsed, payload, "sentinel file content mismatch");
}

/// Dispatching an event with no registered hooks must be a no-op.
#[tokio::test]
async fn dispatch_unknown_event_is_noop() {
    let ext_dir = tempfile::tempdir().unwrap();

    // Extension registers a hook for "tool_call" only.
    let manifest = manifest_with_hooks(vec![ExtensionHook {
        event: "tool_call".into(),
        executable: "./hook.sh".into(), // doesn't need to exist for this test
    }]);
    let ext = LoadedExtension {
        manifest,
        root: ext_dir.path().to_path_buf(),
    };

    let dispatcher = HookDispatcher::from_extensions(&[ext]);

    // "user_message" is not registered — should return immediately without error.
    dispatcher
        .dispatch("user_message", &json!({"text": "hello"}))
        .await;
    // reaching here without panic or error is the assertion.
}

/// A hook pointing at a non-existent executable must not crash the dispatcher;
/// it should log a warning and carry on.
#[tokio::test]
async fn hook_with_missing_executable_does_not_crash() {
    let ext_dir = tempfile::tempdir().unwrap();

    let manifest = manifest_with_hooks(vec![ExtensionHook {
        event: "tool_call".into(),
        executable: "./does-not-exist.sh".into(),
    }]);
    let ext = LoadedExtension {
        manifest,
        root: ext_dir.path().to_path_buf(),
    };

    let dispatcher = HookDispatcher::from_extensions(&[ext]);
    // Must not panic even though the executable is absent.
    dispatcher
        .dispatch("tool_call", &json!({"name": "bash", "input": {}}))
        .await;
}

// ── Step 4: Startup carries extensions unchanged ──────────────────────────────

/// Verify that the `hooks` field survives a round-trip through JSON
/// serialisation (the same path used by `load_one` / `discover`).
#[test]
fn extension_manifest_hooks_round_trip() {
    let manifest = manifest_with_hooks(vec![
        ExtensionHook {
            event: "tool_call".into(),
            executable: "./hooks/on_tool.sh".into(),
        },
        ExtensionHook {
            event: "assistant_message".into(),
            executable: "/usr/local/bin/on_assistant".into(),
        },
    ]);

    let json_str = serde_json::to_string(&manifest).unwrap();
    let decoded: ExtensionManifest = serde_json::from_str(&json_str).unwrap();

    assert_eq!(decoded.hooks.len(), 2);
    assert_eq!(decoded.hooks[0].event, "tool_call");
    assert_eq!(decoded.hooks[0].executable, "./hooks/on_tool.sh");
    assert_eq!(decoded.hooks[1].event, "assistant_message");
    assert_eq!(decoded.hooks[1].executable, "/usr/local/bin/on_assistant");
}

/// `HookDispatcher::from_extensions` with no hooks registered produces an
/// empty dispatcher that is safe to call `dispatch` on.
#[tokio::test]
async fn dispatcher_with_no_hooks_is_safe() {
    let manifest = manifest_with_hooks(vec![]);
    let ext = LoadedExtension {
        manifest,
        root: std::path::PathBuf::from("/tmp"),
    };
    let dispatcher = HookDispatcher::from_extensions(&[ext]);
    // All of these must be no-ops.
    dispatcher.dispatch("tool_call", &json!({})).await;
    dispatcher.dispatch("tool_result", &json!({})).await;
    dispatcher.dispatch("assistant_message", &json!({})).await;
    dispatcher.dispatch("user_message", &json!({})).await;
}
