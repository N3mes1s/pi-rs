//! End-to-end test for `LspWriteTool` against the python fake LSP
//! server (`tests/fake_lsp_server.py`). Proves RFD 0001 wiring:
//!
//! * `format_on_write` runs `textDocument/formatting`, applies the
//!   server's `TextEdit[]` reply, and writes the result back to disk.
//! * `diagnostics_on_write` runs `textDocument/diagnostic` and attaches
//!   the `items` array to `display.diagnostics` without setting
//!   `is_error`.
//!
//! No real `rust-analyzer` dependency — the fake server returns a
//! deterministic `TextEdit` (replace line 0 with "FORMATTED\n") and a
//! single canned diagnostic, so the assertions are byte-exact.

use pi_coding_agent::native::lsp::config::LanguageConfig;
use pi_coding_agent::native::lsp::{LspConfig, LspWriteTool};
use pi_tools::{Tool, ToolContext};
use serde_json::json;

fn fake_server_path() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fake_lsp_server.py")
        .to_string_lossy()
        .into_owned()
}

fn build_cfg(format_on_write: bool, diagnostics_on_write: bool) -> LspConfig {
    let mut cfg = LspConfig::default();
    cfg.enabled = true;
    cfg.format_on_write = format_on_write;
    cfg.diagnostics_on_write = diagnostics_on_write;
    cfg.languages.insert(
        "rust".into(),
        LanguageConfig {
            enabled: Some(true),
            command: Some(vec!["python3".into(), fake_server_path()]),
        },
    );
    cfg
}

fn ctx_for(cwd: &std::path::Path) -> ToolContext {
    ToolContext {
        cwd: cwd.to_path_buf(),
        max_output_bytes: 256 * 1024,
    }
}

#[tokio::test]
async fn format_on_write_replaces_file_with_servers_text_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foo.rs");
    let cfg = build_cfg(true, false);
    let tool = LspWriteTool::new(cfg);
    let res = tool
        .invoke(
            &ctx_for(dir.path()),
            "c1",
            json!({
                "path": path.to_string_lossy(),
                "content": "raw\nbody\n",
            }),
        )
        .await
        .unwrap();
    assert!(
        !res.is_error,
        "write hook must not fail the write: {:?}",
        res.display
    );
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, "FORMATTED\nbody\n",
        "fake server replaces line 0 with FORMATTED\\n; line 1 untouched"
    );
    let display = res.display.expect("display payload");
    assert_eq!(display["formatted"], json!(true));
}

#[tokio::test]
async fn diagnostics_on_write_attaches_items_array_without_marking_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bar.rs");
    let cfg = build_cfg(false, true);
    let tool = LspWriteTool::new(cfg);
    let res = tool
        .invoke(
            &ctx_for(dir.path()),
            "c1",
            json!({
                "path": path.to_string_lossy(),
                "content": "fn main(){}\n",
            }),
        )
        .await
        .unwrap();
    assert!(!res.is_error);
    let display = res.display.expect("display payload");
    let diags = display["diagnostics"].as_array().expect("diagnostics array");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["_marker"], json!("diagnostics"));
}

#[tokio::test]
async fn format_and_diagnostics_compose_in_one_pass() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baz.rs");
    let cfg = build_cfg(true, true);
    let tool = LspWriteTool::new(cfg);
    let res = tool
        .invoke(
            &ctx_for(dir.path()),
            "c1",
            json!({
                "path": path.to_string_lossy(),
                "content": "raw\nbody\n",
            }),
        )
        .await
        .unwrap();
    assert!(!res.is_error);
    let display = res.display.unwrap();
    assert_eq!(display["formatted"], json!(true));
    assert!(display["diagnostics"].as_array().is_some());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FORMATTED\nbody\n"
    );
}
