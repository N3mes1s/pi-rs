//! End-to-end `LspWriteTool` test against the **real** `rust-analyzer`
//! binary.
//!
//! Sister test to `lsp_real_rust_analyzer.rs` — same skip-on-missing
//! semantics, same "wait for indexing" backoff loop. Here we drive the
//! `format_on_write` hook end-to-end: write a deliberately mis-formatted
//! `src/lib.rs` through `LspWriteTool` and check that rust-analyzer's
//! `textDocument/formatting` reply is applied to disk and that the tool
//! result advertises `display.formatted == true`.

use pi_coding_agent::native::lsp::config::LanguageConfig;
use pi_coding_agent::native::lsp::{LspConfig, LspWriteTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

fn require_rust_analyzer() -> Option<std::path::PathBuf> {
    match which::which("rust-analyzer") {
        Ok(p) => Some(p),
        Err(_) => {
            eprintln!("rust-analyzer not on PATH; skipping LspWriteTool real-LSP integration test");
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

[lib]
name = "fixture"
path = "src/lib.rs"
"#,
    )
    .unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let lib_path = src.join("lib.rs");
    // Seed file so rust-analyzer has something to chew on while it
    // crawls the project; the actual badly-formatted content is written
    // through `LspWriteTool` below.
    std::fs::write(&lib_path, "pub fn add(a: u32, b: u32) -> u32 { a + b }\n").unwrap();
    lib_path
}

fn build_tool(ra: &Path) -> LspWriteTool {
    let mut cfg = LspConfig::default();
    cfg.enabled = true;
    cfg.format_on_write = true;
    cfg.diagnostics_on_write = false;
    cfg.languages.insert(
        "rust".into(),
        LanguageConfig {
            enabled: Some(true),
            command: Some(vec![ra.to_string_lossy().into_owned()]),
            format_options: Default::default(),
        },
    );
    LspWriteTool::new(cfg)
}

fn ctx_for(cwd: &Path) -> ToolContext {
    ToolContext {
        cwd: cwd.to_path_buf(),
        max_output_bytes: 1024 * 1024,
    }
}

#[tokio::test]
async fn lsp_write_tool_real_rust_analyzer_format_on_write() {
    let Some(ra) = require_rust_analyzer() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let lib_path = write_fixture_crate(dir.path());
    let tool = build_tool(&ra);

    let badly_formatted = "fn add(a:u32,b:u32)->u32{a+b}\n";

    // rust-analyzer's first response can take 30-60s on a cold registry
    // — we retry the *whole* write up to ~30s on a 200ms cadence, with
    // some extra slack on the very first attempts where the server is
    // still warming.
    let mut formatted_ok = false;
    let mut last_disk = String::new();
    let mut last_display = serde_json::Value::Null;
    for i in 0..150 {
        // Re-stomp the file on every iteration so a partial write from
        // a previous attempt can't fool the assertion.
        let res = tool
            .invoke(
                &ctx_for(dir.path()),
                "c1",
                json!({
                    "path": lib_path.to_string_lossy(),
                    "content": badly_formatted,
                }),
            )
            .await
            .expect("LspWriteTool::invoke should not error");

        if !res.is_error {
            if let Some(d) = res.display.as_ref() {
                last_display = d.clone();
                let on_disk = std::fs::read_to_string(&lib_path).unwrap_or_default();
                last_disk = on_disk.clone();
                let formatted_flag = d
                    .get("formatted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let rustfmt_shaped = on_disk.contains("pub fn add(a: u32, b: u32) -> u32 {")
                    || on_disk.contains("fn add(a: u32, b: u32) -> u32 {");
                if formatted_flag && rustfmt_shaped {
                    formatted_ok = true;
                    break;
                }
            }
        }

        let wait = Duration::from_millis(200 + (i as u64).min(20) * 100);
        tokio::time::sleep(wait).await;
    }

    assert!(
        formatted_ok,
        "rust-analyzer never produced a formatted lib.rs.\nlast on-disk:\n{last_disk}\nlast display: {last_display}"
    );

    let final_disk = std::fs::read_to_string(&lib_path).unwrap();
    assert!(
        final_disk.contains("pub fn add(a: u32, b: u32) -> u32 {")
            || final_disk.contains("fn add(a: u32, b: u32) -> u32 {"),
        "expected rustfmt-shaped signature on disk; got:\n{final_disk}"
    );
    assert_eq!(
        last_display.get("formatted").and_then(|v| v.as_bool()),
        Some(true),
        "display.formatted should be true; display={last_display}"
    );
}
