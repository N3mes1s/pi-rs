//! Smoke tests for `pi --share <session>` (H4).
//!
//! Build a tiny .jsonl session in a tempdir, run the binary against it,
//! and assert that a non-empty .html file was written to
//! `<PI_CODING_AGENT_DIR>/shares/<id>.html`.

use std::process::Command;

fn pi_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

fn write_minimal_session(path: &std::path::Path) {
    // Two entries: a Meta header (so provider/model render correctly)
    // and a User message. Both serialised via SessionEntry's own
    // schema so we can't drift from the real format.
    use pi_agent_core::{SessionEntry, SessionEntryKind};
    use pi_ai::Message;
    let entries = vec![
        SessionEntry {
            id: "1".into(),
            parent_id: None,
            timestamp: 0,
            kind: SessionEntryKind::Meta {
                cwd: ".".into(),
                provider: "anthropic".into(),
                model: "claude-sonnet".into(),
                title: None,
            },
        },
        SessionEntry {
            id: "2".into(),
            parent_id: Some("1".into()),
            timestamp: 1,
            kind: SessionEntryKind::User {
                message: Message::user_text("hello from a share test"),
            },
        },
    ];
    let mut txt = String::new();
    for e in &entries {
        txt.push_str(&serde_json::to_string(e).unwrap());
        txt.push('\n');
    }
    std::fs::write(path, txt).unwrap();
}

#[test]
fn share_with_explicit_path_writes_non_empty_html_in_shares_dir() {
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("abc12345.jsonl");
    write_minimal_session(&session_path);

    let agent_dir = dir.path().join("agent");
    let out = Command::new(pi_binary())
        .args(["--share", session_path.to_str().unwrap()])
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let printed = stdout.trim();
    let printed_path = std::path::Path::new(printed);
    assert!(
        printed_path.exists(),
        "printed path `{printed}` should exist"
    );
    // It should land under <agent_dir>/shares/.
    assert!(
        printed.contains("shares/abc12345.html") || printed.ends_with("abc12345.html"),
        "expected .../shares/abc12345.html, got `{printed}`"
    );
    let contents = std::fs::read_to_string(printed_path).unwrap();
    assert!(!contents.is_empty(), "html file is empty");
    // Confidence checks on what the renderer emitted.
    assert!(contents.contains("<!doctype html>"));
    assert!(contents.contains("hello from a share test"));
    assert!(contents.contains("anthropic/claude-sonnet"));
}

#[test]
fn share_rejects_unknown_id_with_clean_error() {
    let agent_dir = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let out = Command::new(pi_binary())
        .args(["--share", "this-id-does-not-exist"])
        .env("PI_CODING_AGENT_DIR", agent_dir.path())
        .current_dir(cwd.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no session jsonl"),
        "expected `no session jsonl` in stderr, got: {stderr}"
    );
}
