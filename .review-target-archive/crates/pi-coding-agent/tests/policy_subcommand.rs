//! Integration tests for `pi --policy {list,add,deny,allow,remove}`
//! (H3). Each test uses `tempdir` + `PI_CODING_AGENT_DIR` to keep the
//! real `~/.pi/agent/auto-approve.json` untouched.
//!
//! Coverage:
//! - `list` against an empty agent dir prints the safe defaults.
//! - `add bash:<re>` appends to bash.command_allow_regex.
//! - `deny bash:<re>` appends to bash.command_deny_regex.
//! - `allow read:*` flips read.always_approve to true.
//! - `remove bash:<re>` reverses an add.
//! - malformed regex is rejected (non-zero exit, file untouched).
//! - unknown verb is rejected.

use std::process::Command;

fn pi_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

fn pi(dir: &std::path::Path) -> Command {
    let mut c = Command::new(pi_binary());
    c.env("PI_CODING_AGENT_DIR", dir);
    c
}

fn read_policy(dir: &std::path::Path) -> serde_json::Value {
    let txt = std::fs::read_to_string(dir.join("auto-approve.json")).unwrap();
    serde_json::from_str(&txt).unwrap()
}

#[test]
fn list_in_fresh_agent_dir_prints_safe_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let out = pi(dir.path()).args(["--policy", "list"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("policy file:"));
    assert!(stdout.contains("default_decision: Ask"));
    assert!(stdout.contains("read [always_approve]"));
    // `list` is read-only — no file should be written.
    assert!(!dir.path().join("auto-approve.json").exists());
}

#[test]
fn add_appends_pattern_to_command_allow_regex() {
    let dir = tempfile::tempdir().unwrap();
    let out = pi(dir.path())
        .args(["--policy", "add bash:cargo .*"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = read_policy(dir.path());
    let bash_rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool"] == "bash")
        .expect("bash rule should exist");
    let allows = bash_rule["command_allow_regex"].as_array().unwrap();
    assert!(allows.iter().any(|p| p == "cargo .*"));
}

#[test]
fn deny_appends_pattern_to_command_deny_regex() {
    let dir = tempfile::tempdir().unwrap();
    let out = pi(dir.path())
        .args(["--policy", "deny bash:rm -rf /"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = read_policy(dir.path());
    let bash_rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool"] == "bash")
        .unwrap();
    let denies = bash_rule["command_deny_regex"].as_array().unwrap();
    assert!(denies.iter().any(|p| p == "rm -rf /"));
}

#[test]
fn allow_sets_always_approve_true() {
    let dir = tempfile::tempdir().unwrap();
    // Use a tool that isn't in default_safe so we exercise the create
    // branch instead of mutating an existing rule.
    let out = pi(dir.path())
        .args(["--policy", "allow custom_tool:*"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = read_policy(dir.path());
    let rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool"] == "custom_tool")
        .expect("custom_tool rule should be created");
    assert_eq!(rule["always_approve"], serde_json::json!(true));
}

#[test]
fn remove_reverses_an_add() {
    let dir = tempfile::tempdir().unwrap();
    pi(dir.path())
        .args(["--policy", "add bash:cargo .*"])
        .output()
        .unwrap();
    let out = pi(dir.path())
        .args(["--policy", "remove bash:cargo .*"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = read_policy(dir.path());
    let bash_rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool"] == "bash")
        .unwrap();
    let allows = bash_rule["command_allow_regex"].as_array().unwrap();
    assert!(allows.iter().all(|p| p != "cargo .*"));
}

#[test]
fn add_rejects_malformed_regex_without_writing_file() {
    let dir = tempfile::tempdir().unwrap();
    // `(unclosed` is a parse error in the regex crate.
    let out = pi(dir.path())
        .args(["--policy", "add bash:(unclosed"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "should reject malformed regex");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid regex"),
        "expected `invalid regex` in stderr, got: {stderr}"
    );
    assert!(
        !dir.path().join("auto-approve.json").exists(),
        "policy file must not be written when validation fails"
    );
}

#[test]
fn unknown_verb_rejected_with_helpful_message() {
    let dir = tempfile::tempdir().unwrap();
    let out = pi(dir.path())
        .args(["--policy", "destroy everything"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown --policy verb"));
}

#[test]
fn add_is_idempotent_no_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    pi(dir.path())
        .args(["--policy", "add bash:ls"])
        .output()
        .unwrap();
    pi(dir.path())
        .args(["--policy", "add bash:ls"])
        .output()
        .unwrap();
    let v = read_policy(dir.path());
    let bash_rule = v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool"] == "bash")
        .unwrap();
    let allows = bash_rule["command_allow_regex"].as_array().unwrap();
    let count = allows.iter().filter(|p| p.as_str() == Some("ls")).count();
    assert_eq!(count, 1, "add should not create duplicate entries");
}
