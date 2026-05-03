//! Additional coverage for grep / find / ls.

use pi_tools::{ToolContext, ToolRegistry};
use serde_json::json;

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext {
        cwd: dir.to_path_buf(),
        max_output_bytes: 64 * 1024,
    }
}

#[tokio::test]
async fn grep_glob_filter_matches_only_one_of_two_files() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_unsafe_extras();
    let write = reg.get("write").unwrap();
    let grep = reg.get("grep").unwrap();
    let c = ctx(dir.path());

    write
        .invoke(&c, "1", json!({"path": "a.txt", "content": "needle here"}))
        .await
        .unwrap();
    write
        .invoke(
            &c,
            "2",
            json!({"path": "b.md", "content": "needle here too"}),
        )
        .await
        .unwrap();

    let r = grep
        .invoke(&c, "3", json!({"pattern": "needle", "glob": "*.txt"}))
        .await
        .unwrap();
    assert!(!r.is_error);
    assert!(
        r.model_output.contains("a.txt"),
        "expected a.txt in: {}",
        r.model_output
    );
    assert!(
        !r.model_output.contains("b.md"),
        "b.md should be filtered out: {}",
        r.model_output
    );
}

#[tokio::test]
async fn find_honors_max_results() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_unsafe_extras();
    let write = reg.get("write").unwrap();
    let find = reg.get("find").unwrap();
    let c = ctx(dir.path());

    for i in 0..6 {
        write
            .invoke(
                &c,
                &format!("w{i}"),
                json!({"path": format!("f{i}.txt"), "content": "x"}),
            )
            .await
            .unwrap();
    }

    let r = find
        .invoke(&c, "f", json!({"glob": "**/*.txt", "max_results": 2}))
        .await
        .unwrap();
    assert!(!r.is_error);
    let line_count = r
        .model_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert!(
        line_count <= 2,
        "expected ≤2 lines, got {line_count}: {}",
        r.model_output
    );
}

#[tokio::test]
async fn ls_in_nonexistent_directory_returns_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_unsafe_extras();
    let ls = reg.get("ls").unwrap();
    let c = ctx(dir.path());

    let r = ls
        .invoke(&c, "1", json!({"path": "this/does/not/exist"}))
        .await
        .unwrap();
    assert!(r.is_error, "expected is_error: {}", r.model_output);
    assert!(r.model_output.contains("ERROR"));
}
