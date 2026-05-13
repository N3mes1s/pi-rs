//! Regression tests: every read-path tool must cap its returned
//! `model_output` at `ctx.max_output_bytes` and append a truncation marker
//! when it does so.
//!
//! Proposal: "tools-core: cap read/grep/find/ls output to match bash cap"

use pi_tools_core::{
    find::FindTool, grep::GrepTool, ls::LsTool, read::ReadTool, Tool, ToolContext,
};
use serde_json::json;
use std::io::Write;

/// A tiny cap for tests so we don't need huge fixtures.
const TEST_CAP: usize = 32 * 1024; // 32 KiB

fn test_ctx(tmp: &tempfile::TempDir) -> ToolContext {
    ToolContext {
        cwd: tmp.path().to_path_buf(),
        max_output_bytes: TEST_CAP,
    }
}

// ── read ─────────────────────────────────────────────────────────────────────

/// Writing 1 MiB of 'x' bytes, then reading it through the tool, must yield
/// a `model_output` that is ≤ cap bytes and contains the truncation marker.
#[tokio::test]
async fn read_tool_caps_large_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("big.txt");

    // Write 1 MiB of printable ASCII.
    {
        let mut f = std::fs::File::create(&file).unwrap();
        let chunk = b"x".repeat(1024);
        for _ in 0..1024 {
            f.write_all(&chunk).unwrap();
        }
    }

    let tool = ReadTool;
    let ctx = test_ctx(&tmp);
    let result = tool
        .invoke(
            &ctx,
            "call-1",
            json!({"path": file.to_string_lossy().as_ref()}),
        )
        .await
        .expect("invoke must succeed");

    assert!(
        !result.is_error,
        "read of valid file should not be an error"
    );
    assert!(
        result.model_output.len() <= TEST_CAP + 128,
        "model_output ({} bytes) exceeds cap ({} bytes) by more than the marker overhead",
        result.model_output.len(),
        TEST_CAP,
    );
    assert!(
        result.model_output.contains("truncated"),
        "truncation marker must be present; got: {:?}",
        &result.model_output[result.model_output.len().saturating_sub(200)..]
    );
}

// ── grep ─────────────────────────────────────────────────────────────────────

/// A directory full of matching lines should be capped.
#[tokio::test]
async fn grep_tool_caps_many_matches() {
    let tmp = tempfile::tempdir().unwrap();

    // Write enough matching lines to overflow TEST_CAP.
    // Each line is ~80 chars; 2 000 lines ≈ 160 KiB > 32 KiB cap.
    {
        let file = tmp.path().join("matches.txt");
        let mut f = std::fs::File::create(&file).unwrap();
        for i in 0..2_000usize {
            writeln!(f, "MATCH pattern line number {i:06} padding xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").unwrap();
        }
    }

    let tool = GrepTool;
    let ctx = test_ctx(&tmp);
    let result = tool
        .invoke(&ctx, "call-2", json!({"pattern": "MATCH", "max_results": 2000}))
        .await
        .expect("invoke must succeed");

    assert!(!result.is_error);
    assert!(
        result.model_output.len() <= TEST_CAP + 128,
        "grep model_output ({}) exceeds cap ({})",
        result.model_output.len(),
        TEST_CAP,
    );
    assert!(
        result.model_output.contains("truncated"),
        "truncation marker must be present in grep output"
    );
}

// ── find ─────────────────────────────────────────────────────────────────────

/// Many matching files should be capped.
#[tokio::test]
async fn find_tool_caps_many_results() {
    let tmp = tempfile::tempdir().unwrap();

    // Create enough files so that the joined paths overflow TEST_CAP.
    // Each path is ~50 chars; 1 000 files ≈ 50 KiB > 32 KiB cap.
    for i in 0..1_000usize {
        let name = format!("file_{i:06}_padding_xxxxxxxxxxxxxxxxxxxxxxxx.txt");
        std::fs::File::create(tmp.path().join(&name)).unwrap();
    }

    let tool = FindTool;
    let ctx = test_ctx(&tmp);
    let result = tool
        .invoke(&ctx, "call-3", json!({"glob": "*.txt", "max_results": 1000}))
        .await
        .expect("invoke must succeed");

    assert!(!result.is_error);
    assert!(
        result.model_output.len() <= TEST_CAP + 128,
        "find model_output ({}) exceeds cap ({})",
        result.model_output.len(),
        TEST_CAP,
    );
    assert!(
        result.model_output.contains("truncated"),
        "truncation marker must be present in find output"
    );
}

// ── ls ───────────────────────────────────────────────────────────────────────

/// A directory with many long-named entries should also be capped.
#[tokio::test]
async fn ls_tool_caps_many_entries() {
    let tmp = tempfile::tempdir().unwrap();

    // Each entry ≈ 50 chars; 1 000 entries ≈ 50 KiB > 32 KiB cap.
    for i in 0..1_000usize {
        let name = format!("entry_{i:06}_padding_xxxxxxxxxxxxxxxxxxxxxxxx");
        std::fs::File::create(tmp.path().join(&name)).unwrap();
    }

    let tool = LsTool;
    let ctx = test_ctx(&tmp);
    let result = tool
        .invoke(
            &ctx,
            "call-4",
            json!({"path": tmp.path().to_string_lossy().as_ref()}),
        )
        .await
        .expect("invoke must succeed");

    assert!(!result.is_error);
    assert!(
        result.model_output.len() <= TEST_CAP + 128,
        "ls model_output ({}) exceeds cap ({})",
        result.model_output.len(),
        TEST_CAP,
    );
    assert!(
        result.model_output.contains("truncated"),
        "truncation marker must be present in ls output"
    );
}
