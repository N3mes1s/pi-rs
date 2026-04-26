//! Extra coverage for `pi_coding_agent::share`.
//!
//! - exercises every Markdown rendering branch (User / Assistant /
//!   ToolCall / ToolResult / Compaction, plus the skipped Meta/Usage
//!   header items) in a single integration walk-through;
//! - drives `run_gh_gist` with a `PATH` that does not contain `gh` so
//!   the friendly install hint is returned without ever touching the
//!   network.

use pi_agent_core::{SessionEntry, SessionEntryKind};
use pi_ai::{Message, ToolCall, ToolResult, Usage};
use pi_coding_agent::share::{render_markdown, run_gh_gist};

fn entry(id: &str, kind: SessionEntryKind) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: 0,
        kind,
    }
}

#[test]
fn render_markdown_walks_every_branch_and_skips_meta_usage() {
    let branch = vec![
        // Header-info entries — must be skipped by the renderer.
        entry(
            "m",
            SessionEntryKind::Meta {
                cwd: "/tmp/x".into(),
                provider: "anthropic".into(),
                model: "sonnet".into(),
                title: Some("t".into()),
            },
        ),
        entry(
            "sp",
            SessionEntryKind::SystemPrompt {
                text: "hidden system prompt".into(),
            },
        ),
        entry(
            "u",
            SessionEntryKind::Usage {
                usage: Usage::default(),
            },
        ),
        // Conversational entries.
        entry(
            "u1",
            SessionEntryKind::User {
                message: Message::user_text("first user message"),
            },
        ),
        entry(
            "a1",
            SessionEntryKind::Assistant {
                message: Message::assistant_text("first assistant reply"),
            },
        ),
        entry(
            "tc1",
            SessionEntryKind::ToolCall {
                call: ToolCall {
                    id: "tc1".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"cmd": "echo hi"}),
                },
            },
        ),
        entry(
            "tr1",
            SessionEntryKind::ToolResult {
                result: ToolResult {
                    tool_use_id: "tc1".into(),
                    model_output: "hi".into(),
                    display: None,
                    is_error: false,
                },
            },
        ),
        entry(
            "c1",
            SessionEntryKind::Compaction {
                summary: "older messages summarised".into(),
                replaced_ids: vec!["x".into(), "y".into()],
            },
        ),
    ];

    let md = render_markdown("sess-42", "anthropic", "sonnet", &branch);

    // Header line + model.
    assert!(md.starts_with("# session sess-42\n"));
    assert!(md.contains("_model: anthropic/sonnet_"));

    // Hidden header-info kinds must NOT leak into the rendered body.
    assert!(!md.contains("hidden system prompt"));
    assert!(!md.contains("## meta"));
    assert!(!md.contains("## usage"));

    // Conversational kinds appear in order with the expected headings.
    let user = md.find("## user").expect("user heading");
    let assistant = md.find("## assistant").expect("assistant heading");
    let tool = md.find("## tool: shell").expect("tool heading");
    let tool_result = md.find("## tool result").expect("tool-result heading");
    let compaction = md.find("## compaction").expect("compaction heading");
    assert!(user < assistant);
    assert!(assistant < tool);
    assert!(tool < tool_result);
    assert!(tool_result < compaction);

    // Actual content slices.
    assert!(md.contains("first user message"));
    assert!(md.contains("first assistant reply"));
    assert!(md.contains("\"cmd\": \"echo hi\""));
    assert!(md.contains("```\nhi\n```"));
    assert!(md.contains("older messages summarised"));
}

#[test]
fn render_markdown_with_empty_branch_emits_only_the_header() {
    let md = render_markdown("only-header", "openai", "gpt-4o", &[]);
    let want = "# session only-header\n_model: openai/gpt-4o_\n\n";
    assert_eq!(md, want);
}

#[test]
fn run_gh_gist_returns_friendly_error_when_gh_is_missing() {
    // Build a tempdir that contains nothing called `gh`, then point
    // PATH at *only* that directory. `which("gh")` will fail and the
    // helper must surface its install hint, not panic, not spawn.
    let empty = tempfile::tempdir().expect("tempdir");
    // SAFETY: `set_var` is unsafe in newer std editions; tests run
    // single-threaded under `--test-threads=1` (see scripts/coverage.sh)
    // so this is fine.
    let prev_path = std::env::var_os("PATH");
    unsafe {
        std::env::set_var("PATH", empty.path());
    }

    let err = run_gh_gist("# body").expect_err("must fail without gh");
    assert!(
        err.contains("gh") && err.contains("cli.github.com"),
        "expected install hint, got: {err}"
    );

    // Restore PATH so other tests are not affected.
    unsafe {
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }
}
