//! `/share` helper — renders the active branch of a session as Markdown
//! and uploads it as a GitHub Gist via the `gh` CLI.
//!
//! Also provides [`render_session_html`] for the `/export` command which
//! produces a self-contained HTML document with role-coloured blocks.
//!
//! Pure rendering is exposed via [`render_markdown`] and
//! [`render_session_html`] so they can be unit-tested without a real
//! session manager or process invocation.

use pi_agent_core::{SessionEntry, SessionEntryKind};

/// Render a session branch as the Markdown layout used by `/share`:
///
/// ```text
/// # session <id>
/// _model: <provider>/<model>_
///
/// ## user
/// <text>
///
/// ## assistant
/// <text>
///
/// ## tool: <name>
/// <input>
///
/// ## tool result
/// <output>
/// ```
pub fn render_markdown(
    session_id: &str,
    provider: &str,
    model: &str,
    branch: &[SessionEntry],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# session {session_id}\n"));
    out.push_str(&format!("_model: {provider}/{model}_\n\n"));
    for e in branch {
        match &e.kind {
            SessionEntryKind::User { message } => {
                out.push_str("## user\n");
                out.push_str(message.text().trim_end());
                out.push_str("\n\n");
            }
            SessionEntryKind::Assistant { message } => {
                out.push_str("## assistant\n");
                out.push_str(message.text().trim_end());
                out.push_str("\n\n");
            }
            SessionEntryKind::ToolCall { call } => {
                out.push_str(&format!("## tool: {}\n", call.name));
                let input =
                    serde_json::to_string_pretty(&call.input).unwrap_or_else(|_| "{}".into());
                out.push_str("```json\n");
                out.push_str(input.trim_end());
                out.push_str("\n```\n\n");
            }
            SessionEntryKind::ToolResult { result } => {
                out.push_str("## tool result\n");
                out.push_str("```\n");
                out.push_str(result.model_output.trim_end());
                out.push_str("\n```\n\n");
            }
            SessionEntryKind::Compaction { summary, .. } => {
                out.push_str("## compaction\n");
                out.push_str(summary.trim_end());
                out.push_str("\n\n");
            }
            // Meta / SystemPrompt / Usage are header info — already covered by
            // the title line; don't repeat.
            _ => {}
        }
    }
    out
}

// ─── HTML export ─────────────────────────────────────────────────────────────

/// HTML-escape `s`: replace `&`, `<`, `>`, and `"` so the text is safe to
/// embed in HTML attributes and CDATA.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// Render a session branch as a self-contained HTML document.
///
/// Each conversational block is wrapped in a `<div class="block role-X">`
/// element with a `<header>` showing the role and (for tool calls) the tool
/// name.  The message body is placed in a `<pre>` so whitespace is preserved.
///
/// The document includes a small embedded CSS block with role-coloured left
/// borders:
/// - `role-user`        → cyan  (`#00bcd4`)
/// - `role-assistant`   → green (`#4caf50`)
/// - `role-tool_call`   → yellow(`#ffc107`)
/// - `role-tool_result` → dark grey (`#607d8b`)
/// - `role-compaction`  → orange (`#ff9800`)
///
/// All text bodies are HTML-escaped before insertion.
pub fn render_session_html(
    messages: &[SessionEntry],
    session_id: &str,
    provider: &str,
    model: &str,
) -> String {
    let short_id = if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    };

    let mut body = String::new();

    for e in messages {
        match &e.kind {
            SessionEntryKind::User { message } => {
                let text = html_escape(message.text().trim_end());
                body.push_str(&format!(
                    "<div class=\"block role-user\">\
                      <header>user</header>\
                      <pre>{text}</pre>\
                    </div>\n"
                ));
            }
            SessionEntryKind::Assistant { message } => {
                let text = html_escape(message.text().trim_end());
                body.push_str(&format!(
                    "<div class=\"block role-assistant\">\
                      <header>assistant</header>\
                      <pre>{text}</pre>\
                    </div>\n"
                ));
            }
            SessionEntryKind::ToolCall { call } => {
                let name = html_escape(&call.name);
                let input = serde_json::to_string_pretty(&call.input).unwrap_or_else(|_| "{}".into());
                let input_escaped = html_escape(input.trim_end());
                body.push_str(&format!(
                    "<div class=\"block role-tool_call\">\
                      <header>tool_call: {name}</header>\
                      <pre>{input_escaped}</pre>\
                    </div>\n"
                ));
            }
            SessionEntryKind::ToolResult { result } => {
                let output = html_escape(result.model_output.trim_end());
                let extra_class = if result.is_error { " role-error" } else { "" };
                body.push_str(&format!(
                    "<div class=\"block role-tool_result{extra_class}\">\
                      <header>tool_result</header>\
                      <pre>{output}</pre>\
                    </div>\n"
                ));
            }
            SessionEntryKind::Compaction { summary, .. } => {
                let text = html_escape(summary.trim_end());
                body.push_str(&format!(
                    "<div class=\"block role-compaction\">\
                      <header>compaction</header>\
                      <pre>{text}</pre>\
                    </div>\n"
                ));
            }
            // Meta / SystemPrompt / Usage are header info; skip.
            _ => {}
        }
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>pi-rs session {short_id}</title>
<style>
body {{ font-family: monospace; background: #1e1e1e; color: #d4d4d4; margin: 1rem 2rem; }}
.block {{ border-left: 4px solid #555; margin: 0.75rem 0; padding: 0.5rem 1rem; background: #252526; border-radius: 2px; }}
.block header {{ font-weight: bold; margin-bottom: 0.25rem; font-size: 0.85em; opacity: 0.8; }}
.block pre {{ margin: 0; white-space: pre-wrap; word-break: break-word; }}
.role-user        {{ border-color: #00bcd4; }}
.role-assistant   {{ border-color: #4caf50; }}
.role-tool_call   {{ border-color: #ffc107; }}
.role-tool_result {{ border-color: #607d8b; }}
.role-compaction  {{ border-color: #ff9800; }}
.role-error       {{ border-color: #f44336; }}
h1 {{ color: #9cdcfe; margin-bottom: 0.25rem; }}
p.meta {{ color: #888; margin-top: 0; font-size: 0.9em; }}
</style>
</head>
<body>
<h1>pi-rs session {session_id}</h1>
<p class="meta">{provider}/{model}</p>
{body}</body>
</html>
"#,
        short_id = short_id,
        session_id = html_escape(session_id),
        provider = html_escape(provider),
        model = html_escape(model),
        body = body,
    )
}

/// Run `gh gist create -d 'pi-rs session' -` with `body` on stdin.
/// Returns the captured stdout (URL) on success.
pub fn run_gh_gist(body: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if which::which("gh").is_err() {
        return Err(
            "/share requires the `gh` CLI; install it from https://cli.github.com".into(),
        );
    }
    let mut child = Command::new("bash")
        .arg("-c")
        .arg("gh gist create -d 'pi-rs session' -")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn gh: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("write gh stdin: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("wait gh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::SessionEntry;
    use pi_ai::{Message, ToolCall, ToolResult};

    fn entry(id: &str, kind: SessionEntryKind) -> SessionEntry {
        SessionEntry {
            id: id.into(),
            parent_id: None,
            timestamp: 0,
            kind,
        }
    }

    #[test]
    fn render_markdown_emits_header_and_each_kind_in_order() {
        let branch = vec![
            entry(
                "1",
                SessionEntryKind::User {
                    message: Message::user_text("hello"),
                },
            ),
            entry(
                "2",
                SessionEntryKind::Assistant {
                    message: Message::assistant_text("hi back"),
                },
            ),
            entry(
                "3",
                SessionEntryKind::ToolCall {
                    call: ToolCall {
                        id: "t".into(),
                        name: "shell".into(),
                        input: serde_json::json!({"cmd": "ls"}),
                    },
                },
            ),
            entry(
                "4",
                SessionEntryKind::ToolResult {
                    result: ToolResult {
                        tool_use_id: "t".into(),
                        model_output: "file.txt".into(),
                        display: None,
                        is_error: false,
                    },
                },
            ),
        ];
        let md = render_markdown("abc-123", "anthropic", "sonnet", &branch);
        assert!(md.starts_with("# session abc-123\n"));
        assert!(md.contains("_model: anthropic/sonnet_"));
        // Section order is preserved.
        let user_pos = md.find("## user").unwrap();
        let asst_pos = md.find("## assistant").unwrap();
        let tool_pos = md.find("## tool: shell").unwrap();
        let res_pos = md.find("## tool result").unwrap();
        assert!(user_pos < asst_pos);
        assert!(asst_pos < tool_pos);
        assert!(tool_pos < res_pos);
        assert!(md.contains("hello"));
        assert!(md.contains("hi back"));
        assert!(md.contains("\"cmd\": \"ls\""));
        assert!(md.contains("file.txt"));
    }

    #[test]
    fn render_markdown_skips_meta_and_usage() {
        let branch = vec![
            entry(
                "1",
                SessionEntryKind::Meta {
                    cwd: ".".into(),
                    provider: "p".into(),
                    model: "m".into(),
                    title: None,
                },
            ),
            entry(
                "2",
                SessionEntryKind::Usage {
                    usage: pi_ai::Usage::default(),
                },
            ),
            entry(
                "3",
                SessionEntryKind::User {
                    message: Message::user_text("only-user"),
                },
            ),
        ];
        let md = render_markdown("s", "p", "m", &branch);
        assert!(!md.contains("## meta"));
        assert!(!md.contains("## usage"));
        assert!(md.contains("only-user"));
    }

    #[test]
    fn render_markdown_includes_compaction_summary() {
        let branch = vec![entry(
            "1",
            SessionEntryKind::Compaction {
                summary: "older context summarised".into(),
                replaced_ids: vec!["a".into(), "b".into()],
            },
        )];
        let md = render_markdown("x", "p", "m", &branch);
        assert!(md.contains("## compaction"));
        assert!(md.contains("older context summarised"));
    }
}
