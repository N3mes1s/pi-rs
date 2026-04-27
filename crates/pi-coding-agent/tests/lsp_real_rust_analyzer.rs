//! End-to-end LSP test against the **real** `rust-analyzer` binary.
//!
//! Why: the existing fake-server tests prove the JSON-RPC framing + tool
//! dispatch is correct. They don't prove rust-analyzer actually answers
//! the way we expect. This test runs every H1 op (and the legacy ones)
//! against a tempdir Cargo project and asserts on the live response.
//!
//! Skip semantics: when `rust-analyzer` isn't on PATH, the test prints
//! a notice and returns early. CI without rust-analyzer doesn't fail;
//! local dev / sandboxes with rust-analyzer installed exercise the real
//! path.
//!
//! Indexing latency: rust-analyzer takes a few seconds to crawl even a
//! one-file crate. We retry each op up to 30 times with 200ms backoff.

use pi_coding_agent::native::lsp::config::LanguageConfig;
use pi_coding_agent::native::lsp::{LspConfig, LspTool};
use pi_tools::{Tool, ToolContext};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

fn require_rust_analyzer() -> Option<std::path::PathBuf> {
    match which::which("rust-analyzer") {
        Ok(p) => Some(p),
        Err(_) => {
            eprintln!("rust-analyzer not on PATH; skipping real-LSP integration test");
            None
        }
    }
}

fn write_fixture_crate(dir: &Path) -> std::path::PathBuf {
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "fixture"
path = "src/main.rs"
"#,
    )
    .unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let main_path = src.join("main.rs");
    std::fs::write(
        &main_path,
        "/// Add two numbers.\n\
         pub fn add(a: u32, b: u32) -> u32 {\n\
         \x20   a + b\n\
         }\n\
         \n\
         fn main() {\n\
         \x20   let _ = add(1, 2);\n\
         }\n",
    )
    .unwrap();
    main_path
}

fn build_tool(_cwd: &Path, ra: &Path) -> LspTool {
    let mut cfg = LspConfig::default();
    cfg.enabled = true;
    cfg.languages.insert(
        "rust".into(),
        LanguageConfig {
            enabled: Some(true),
            command: Some(vec![ra.to_string_lossy().into_owned()]),
            format_options: Default::default(),
        },
    );
    LspTool::new(cfg)
}

fn ctx_for(cwd: &Path) -> ToolContext {
    ToolContext {
        cwd: cwd.to_path_buf(),
        max_output_bytes: 1024 * 1024,
    }
}

async fn invoke_until<F>(
    tool: &LspTool,
    cwd: &Path,
    input: Value,
    n: usize,
    ok: F,
) -> Option<Value>
where
    F: Fn(&Value) -> bool,
{
    for i in 0..n {
        if let Ok(res) = tool.invoke(&ctx_for(cwd), "c1", input.clone()).await {
            if !res.is_error {
                if let Some(d) = res.display.as_ref() {
                    if ok(&d["result"]) {
                        return Some(d["result"].clone());
                    }
                }
            }
        }
        // rust-analyzer needs to run `cargo metadata` + `cargo check` on
        // first contact with a fresh project; that can take 30-60s on a
        // cold registry cache. Long backoff up to 1s between attempts.
        let wait = Duration::from_millis(500 + (i as u64) * 100);
        tokio::time::sleep(wait).await;
    }
    None
}

// Self-skipping when `rust-analyzer` isn't on PATH (CI without the
// component). Otherwise spawns a real RA instance against a tempdir
// crate and exercises every op end-to-end.
#[tokio::test]
async fn real_rust_analyzer_round_trip() {
    let Some(ra) = require_rust_analyzer() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let main_rs = write_fixture_crate(dir.path());
    let tool = build_tool(dir.path(), &ra);

    // ── 1. symbols: should list `add` and `main`.
    // First call is slow because rust-analyzer's `cargo metadata` round
    // trip can take 30-60s on a cold registry. Subsequent ops are fast
    // (server is warm).
    let symbols = invoke_until(
        &tool,
        dir.path(),
        json!({
            "op": "symbols",
            "path": main_rs.to_string_lossy(),
        }),
        90,
        |result| {
            let arr = result.as_array();
            arr.map_or(false, |xs| {
                xs.iter().any(|s| {
                    matches!(
                        s.get("name").and_then(|n| n.as_str()),
                        Some("add" | "main")
                    )
                })
            })
        },
    )
    .await
    .expect("rust-analyzer should return symbols for fixture");

    let names: Vec<String> = symbols
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|n| n == "add"),
        "expected `add` in symbol list, got: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "main"),
        "expected `main` in symbol list, got: {names:?}"
    );

    // ── 2. definition: cursor on `add(1, 2)` → points at fn def.
    // RA may return `[]` while semantic analysis is still warming up;
    // require a non-empty payload before we believe the result.
    let definition = invoke_until(
        &tool,
        dir.path(),
        json!({
            "op": "definition",
            "path": main_rs.to_string_lossy(),
            "line": 6,
            "col": 12,
        }),
        90,
        |result| match result {
            Value::Array(xs) => !xs.is_empty(),
            Value::Object(_) => true,
            _ => false,
        },
    )
    .await
    .expect("rust-analyzer should return a definition for `add(1,2)` call");

    let target_uri = first_uri(&definition).unwrap_or_default();
    assert!(
        target_uri.ends_with("main.rs"),
        "definition should point inside main.rs; got '{target_uri}' raw={definition}"
    );

    // ── 3. hover: cursor on `add` definition.
    let hover = invoke_until(
        &tool,
        dir.path(),
        json!({
            "op": "hover",
            "path": main_rs.to_string_lossy(),
            "line": 1,
            "col": 8,
        }),
        20,
        |result| !result.is_null(),
    )
    .await
    .expect("rust-analyzer should hover for `add` definition");

    let hover_blob = serde_json::to_string(&hover).unwrap();
    assert!(
        hover_blob.contains("add") || hover_blob.contains("Add"),
        "hover should mention `add` or its docstring; got {hover_blob}"
    );

    // ── 4. references: at least 2 hits (def + call site).
    let references = invoke_until(
        &tool,
        dir.path(),
        json!({
            "op": "references",
            "path": main_rs.to_string_lossy(),
            "line": 1,
            "col": 8,
        }),
        20,
        |result| result.as_array().map_or(false, |xs| xs.len() >= 2),
    )
    .await
    .expect("rust-analyzer should return at least 2 references for `add`");

    let refs_count = references.as_array().map(|xs| xs.len()).unwrap_or(0);
    assert!(
        refs_count >= 2,
        "expected ≥2 references for `add`, got {refs_count}"
    );

    // ── 5. rename: workspace edit covering both occurrences.
    let rename = invoke_until(
        &tool,
        dir.path(),
        json!({
            "op": "rename",
            "path": main_rs.to_string_lossy(),
            "line": 1,
            "col": 8,
            "new_name": "sum",
        }),
        20,
        |result| {
            result.is_object()
                && (result.get("changes").is_some()
                    || result.get("documentChanges").is_some())
        },
    )
    .await
    .expect("rust-analyzer should propose a workspace edit for rename");

    let serialised = serde_json::to_string(&rename).unwrap();
    assert!(
        serialised.contains("sum"),
        "rename edit should carry the new name `sum`; got {serialised}"
    );
}

fn first_uri(value: &Value) -> Option<String> {
    match value {
        Value::Array(xs) => xs.first().and_then(first_uri),
        Value::Object(map) => map
            .get("uri")
            .or_else(|| map.get("targetUri"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    }
}
