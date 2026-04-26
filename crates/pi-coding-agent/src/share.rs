//! `/share` helper — renders the active branch of a session as Markdown
//! and uploads it as a GitHub Gist via the `gh` CLI.
//!
//! Pure rendering is exposed via [`render_markdown`] so it can be
//! unit-tested without a real session manager or process invocation.

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
