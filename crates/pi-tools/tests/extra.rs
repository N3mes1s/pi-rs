//! Extra coverage to push pi-tools modules over 90%.
//!
//! These tests exercise the `Tool::read_only()` and `Tool::spec()` paths,
//! the explicit-`cwd` branches, parameter-validation error paths, and the
//! `truncate_for_model` boundary cases.

use pi_tools::{ToolContext, ToolRegistry};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext {
        cwd: dir.to_path_buf(),
        max_output_bytes: 64 * 1024,
    }
}

#[test]
fn tool_context_default_uses_current_dir_and_256kb_cap() {
    let c = ToolContext::default();
    assert_eq!(c.max_output_bytes, 256 * 1024);
    assert!(c.cwd.is_absolute() || c.cwd == PathBuf::from("."));
}

#[test]
fn registry_new_is_empty_and_specs_match_names() {
    let r = ToolRegistry::new();
    assert!(r.names().is_empty());
    assert!(r.specs().is_empty());
}

#[test]
fn registry_keep_only_filters_to_named_tools() {
    let mut r = ToolRegistry::with_extras();
    let before = r.names();
    assert!(before.len() >= 4, "with_extras should bring multiple tools");
    r.keep_only(&["read".into(), "bash".into()]);
    let after = r.names();
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|n| n == "read"));
    assert!(after.iter().any(|n| n == "bash"));
}

#[test]
fn registry_unregister_removes_named_tool() {
    let mut r = ToolRegistry::with_defaults();
    assert!(r.get("bash").is_some());
    r.unregister("bash");
    assert!(r.get("bash").is_none());
    // Still has read/write/edit
    assert!(r.get("read").is_some());
}

#[test]
fn registry_specs_returns_one_spec_per_registered_tool() {
    let r = ToolRegistry::with_extras();
    let names = r.names();
    let specs = r.specs();
    assert_eq!(names.len(), specs.len());
}

#[test]
fn read_only_flags_are_correct_for_each_builtin() {
    let r = ToolRegistry::with_extras();
    // Read-only: read, grep, find, ls
    for name in ["read", "grep", "find", "ls"] {
        let t = r.get(name).unwrap();
        assert!(t.read_only(), "{name} should be read-only");
    }
    // Mutating: write, edit, bash
    for name in ["write", "edit", "bash"] {
        let t = r.get(name).unwrap();
        assert!(!t.read_only(), "{name} should not be read-only");
    }
}

#[test]
fn spec_accessor_returns_correct_name_per_tool() {
    let r = ToolRegistry::with_extras();
    for name in ["read", "write", "edit", "bash", "grep", "find", "ls"] {
        let t = r.get(name).unwrap();
        assert_eq!(t.spec().name, name);
        // every tool should at least describe itself in non-empty terms
        assert!(
            !t.spec().description.is_empty(),
            "{name} has empty description"
        );
    }
}

// --- truncate_for_model boundary cases (exercised via the read tool) ----

#[tokio::test]
async fn read_tool_does_not_truncate_when_file_fits_within_cap() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tiny.txt");
    std::fs::write(&p, "abc").unwrap();
    let c = ToolContext {
        cwd: dir.path().to_path_buf(),
        max_output_bytes: 64 * 1024,
    };
    let r = ToolRegistry::with_defaults();
    let read = r.get("read").unwrap();
    let out = read
        .invoke(&c, "1", json!({"path": "tiny.txt"}))
        .await
        .unwrap();
    assert!(!out.model_output.contains("truncated"));
}

#[tokio::test]
async fn read_tool_truncates_just_above_cap_with_marker() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("over.txt");
    // Exactly 4097 bytes — just above a 4 KiB cap.
    let body = "a".repeat(4097);
    std::fs::write(&p, &body).unwrap();
    let c = ToolContext {
        cwd: dir.path().to_path_buf(),
        max_output_bytes: 4096,
    };
    let r = ToolRegistry::with_defaults();
    let read = r.get("read").unwrap();
    let out = read
        .invoke(&c, "1", json!({"path": "over.txt"}))
        .await
        .unwrap();
    assert!(
        out.model_output.contains("truncated"),
        "got: {}",
        &out.model_output[..out.model_output.len().min(80)]
    );
}

#[tokio::test]
async fn read_tool_offset_skips_initial_lines_and_limit_caps_count() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("multi.txt");
    let body: String = (1..=10)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&p, &body).unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let read = r.get("read").unwrap();
    // offset=4, limit=2 → only line4 and line5 should appear
    let out = read
        .invoke(
            &c,
            "1",
            json!({"path": "multi.txt", "offset": 4, "limit": 2}),
        )
        .await
        .unwrap();
    assert!(out.model_output.contains("line4"));
    assert!(out.model_output.contains("line5"));
    assert!(
        !out.model_output.contains("line1"),
        "offset should skip line1: {}",
        out.model_output
    );
    assert!(
        !out.model_output.contains("line6"),
        "limit should stop at line5: {}",
        out.model_output
    );
}

#[tokio::test]
async fn read_tool_missing_path_returns_invalid_input_error() {
    let c = ctx(std::env::temp_dir().as_path());
    let r = ToolRegistry::with_defaults();
    let read = r.get("read").unwrap();
    let err = read.invoke(&c, "1", json!({})).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("missing"), "msg: {msg}");
}

// --- write boundaries ---

#[tokio::test]
async fn write_tool_creates_nested_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let write = r.get("write").unwrap();
    let out = write
        .invoke(
            &c,
            "1",
            json!({"path": "deep/nested/dir/file.txt", "content": "hi"}),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(dir.path().join("deep/nested/dir/file.txt").is_file());
}

#[tokio::test]
async fn write_tool_missing_path_or_content_errors() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let write = r.get("write").unwrap();
    let e = write
        .invoke(&c, "1", json!({"content": "x"}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("path"));
    let e = write
        .invoke(&c, "1", json!({"path": "x"}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("content"));
}

#[tokio::test]
async fn write_tool_reports_updated_when_target_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let write = r.get("write").unwrap();
    write
        .invoke(&c, "1", json!({"path": "a.txt", "content": "first"}))
        .await
        .unwrap();
    let r2 = write
        .invoke(&c, "2", json!({"path": "a.txt", "content": "second"}))
        .await
        .unwrap();
    assert!(r2.model_output.starts_with("Updated"));
}

// --- edit branches ---

#[tokio::test]
async fn edit_tool_replace_all_replaces_every_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    r.get("write")
        .unwrap()
        .invoke(&c, "1", json!({"path": "x", "content": "a a a a"}))
        .await
        .unwrap();
    let res = r
        .get("edit")
        .unwrap()
        .invoke(
            &c,
            "2",
            json!({"path": "x", "old_string": "a", "new_string": "B", "replace_all": true}),
        )
        .await
        .unwrap();
    assert!(!res.is_error);
    let body = std::fs::read_to_string(dir.path().join("x")).unwrap();
    assert_eq!(body, "B B B B");
}

#[tokio::test]
async fn edit_tool_old_string_not_found_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    r.get("write")
        .unwrap()
        .invoke(&c, "1", json!({"path": "x", "content": "hello"}))
        .await
        .unwrap();
    let res = r
        .get("edit")
        .unwrap()
        .invoke(
            &c,
            "2",
            json!({"path": "x", "old_string": "MISSING", "new_string": "z"}),
        )
        .await
        .unwrap();
    assert!(res.is_error);
    assert!(res.model_output.contains("not found"));
}

#[tokio::test]
async fn edit_tool_missing_required_inputs_each_error() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let edit = r.get("edit").unwrap();
    for missing in [
        json!({}),
        json!({"path": "x"}),
        json!({"path": "x", "old_string": "y"}),
    ] {
        let e = edit.invoke(&c, "1", missing).await.unwrap_err();
        assert!(e.to_string().contains("missing"));
    }
}

// --- bash explicit cwd + missing command ---

#[tokio::test]
async fn bash_tool_uses_explicit_cwd_param_when_provided() {
    // Ensure we land in `subdir` rather than the registry's default cwd.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("subdir/marker.txt"), "marker-here").unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let bash = r.get("bash").unwrap();
    let out = bash
        .invoke(&c, "1", json!({"command": "ls", "cwd": "subdir"}))
        .await
        .unwrap();
    assert!(
        out.model_output.contains("marker.txt"),
        "ls should run in subdir, got: {}",
        out.model_output
    );
    if let Some(d) = &out.display {
        let cwd_field = d.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        assert!(cwd_field.ends_with("subdir"), "cwd display: {cwd_field}");
    }
}

#[tokio::test]
async fn bash_tool_missing_command_errors() {
    let c = ctx(std::env::temp_dir().as_path());
    let r = ToolRegistry::with_defaults();
    let bash = r.get("bash").unwrap();
    let e = bash.invoke(&c, "1", json!({})).await.unwrap_err();
    assert!(e.to_string().contains("command"));
}

#[tokio::test]
async fn bash_tool_failing_command_reports_is_error_with_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let bash = r.get("bash").unwrap();
    let out = bash
        .invoke(&c, "1", json!({"command": "false"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.model_output.contains("[exit"));
}

#[tokio::test]
async fn bash_tool_stderr_only_command_includes_stderr_marker() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_defaults();
    let bash = r.get("bash").unwrap();
    let out = bash
        .invoke(&c, "1", json!({"command": "echo onerr 1>&2"}))
        .await
        .unwrap();
    assert!(out.model_output.contains("[stderr]"));
}

// --- ls branches ---

#[tokio::test]
async fn ls_tool_sorts_entries_and_appends_slash_to_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("zsub")).unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.path().join("beta.txt"), "b").unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let ls = r.get("ls").unwrap();
    let out = ls.invoke(&c, "1", json!({})).await.unwrap();
    assert!(!out.is_error);
    let lines: Vec<&str> = out.model_output.lines().collect();
    // Sorted: alpha.txt, beta.txt, zsub/
    assert_eq!(lines, vec!["alpha.txt", "beta.txt", "zsub/"]);
}

// --- find / grep error paths ---

#[tokio::test]
async fn find_missing_glob_errors_and_invalid_glob_errors() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let find = r.get("find").unwrap();
    let e = find.invoke(&c, "1", json!({})).await.unwrap_err();
    assert!(e.to_string().contains("glob"));
    let e2 = find
        .invoke(&c, "2", json!({"glob": "[bad"}))
        .await
        .unwrap_err();
    assert!(!e2.to_string().is_empty(), "got: {e2:?}");
}

#[tokio::test]
async fn find_with_no_matches_returns_marker() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let find = r.get("find").unwrap();
    let out = find
        .invoke(&c, "1", json!({"glob": "**/*.never"}))
        .await
        .unwrap();
    assert!(out.model_output.contains("(no matches)"));
}

#[tokio::test]
async fn find_with_explicit_path_searches_that_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("inner")).unwrap();
    std::fs::write(dir.path().join("inner/x.txt"), "x").unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let find = r.get("find").unwrap();
    let out = find
        .invoke(&c, "1", json!({"glob": "**/*.txt", "path": "inner"}))
        .await
        .unwrap();
    assert!(out.model_output.contains("x.txt"));
}

#[tokio::test]
async fn grep_missing_pattern_errors_and_invalid_regex_errors() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let grep = r.get("grep").unwrap();
    let e = grep.invoke(&c, "1", json!({})).await.unwrap_err();
    assert!(e.to_string().contains("pattern"));
    let e2 = grep
        .invoke(&c, "2", json!({"pattern": "[bad"}))
        .await
        .unwrap_err();
    assert!(!e2.to_string().is_empty());
}

#[tokio::test]
async fn grep_with_explicit_path_and_no_matches_returns_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "nothing").unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let grep = r.get("grep").unwrap();
    let out = grep
        .invoke(&c, "1", json!({"pattern": "WONT_MATCH", "path": "."}))
        .await
        .unwrap();
    assert!(out.model_output.contains("(no matches)"));
}

#[tokio::test]
async fn grep_invalid_glob_errors() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let r = ToolRegistry::with_extras();
    let grep = r.get("grep").unwrap();
    let e = grep
        .invoke(&c, "1", json!({"pattern": "x", "glob": "[bad"}))
        .await
        .unwrap_err();
    assert!(!e.to_string().is_empty());
}

// --- resolve_path absolute branch (via read tool) ---

#[tokio::test]
async fn read_tool_resolves_absolute_path_directly() {
    let dir = tempfile::tempdir().unwrap();
    let abs = dir.path().join("abs.txt");
    std::fs::write(&abs, "absolute body").unwrap();
    // Set cwd to a different dir to prove the absolute path wins.
    let other = tempfile::tempdir().unwrap();
    let c = ctx(other.path());
    let r = ToolRegistry::with_defaults();
    let read = r.get("read").unwrap();
    let out = read
        .invoke(&c, "1", json!({"path": abs.display().to_string()}))
        .await
        .unwrap();
    assert!(out.model_output.contains("absolute body"));
}

#[tokio::test]
async fn registry_register_replaces_same_named_tool() {
    // We already have BashTool; register a second tool with the same name to
    // force the BTreeMap insert path.
    use pi_ai::ToolSpec;
    use pi_tools::{Tool as ToolTrait, ToolError};
    use serde_json::Value;

    struct Stub;
    #[async_trait::async_trait]
    impl ToolTrait for Stub {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "bash".into(),
                description: "stub".into(),
                input_schema: json!({"type": "object"}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _ctx: &ToolContext,
            id: &str,
            _input: Value,
        ) -> Result<pi_ai::ToolResult, ToolError> {
            Ok(pi_ai::ToolResult {
                tool_use_id: id.into(),
                model_output: "stub".into(),
                display: None,
                is_error: false,
            })
        }
    }

    let mut r = ToolRegistry::with_defaults();
    // The Stub's spec().name == "bash" — intentional override.
    r.register_or_replace(Arc::new(Stub));
    let bash = r.get("bash").unwrap();
    assert!(bash.read_only(), "stub should now be in place of real bash");
}
