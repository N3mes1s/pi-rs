//! Trajectory flamegraph renderer (G11).
//!
//! Renders a session branch as a self-contained HTML page where:
//! - Width is proportional to estimated token cost (~ chars/4 when no
//!   `Usage` entries are present; sums of `Usage.input_tokens +
//!   output_tokens` when they are).
//! - Depth is the natural nesting: each User → Assistant cycle is a
//!   "turn"; tool_call + tool_result entries that appear inside the
//!   assistant's response are nested under that assistant block.
//! - Colour encodes block kind (user, assistant, thinking, tool_call,
//!   tool_result, error).
//!
//! The output is one self-contained `<!DOCTYPE html>` document with
//! embedded CSS — no external assets, no JS. Suitable for `pi
//! --flamegraph <session-id> > flame.html && xdg-open flame.html`.

use pi_agent_core::{SessionEntry, SessionEntryKind};

/// Render a session branch as a complete HTML document.
pub fn render(session_id: &str, branch: &[SessionEntry]) -> String {
    let total = total_tokens(branch).max(1) as f64;
    let body = render_body(branch, total);

    let short_id = if session_id.len() > 12 {
        &session_id[..12]
    } else {
        session_id
    };

    format!(
        "<!DOCTYPE html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<title>pi trajectory — {short_id}</title>
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', monospace;
    background: #0e1117;
    color: #c9d1d9;
    margin: 0;
    padding: 1.5em;
  }}
  h1 {{ font-size: 1.1em; font-weight: 600; margin: 0 0 1em 0; }}
  .legend {{ font-size: 0.85em; margin-bottom: 1em; opacity: 0.8; }}
  .legend span {{
    display: inline-block;
    padding: 2px 8px;
    margin-right: 6px;
    border-radius: 3px;
    font-size: 0.8em;
  }}
  .turn {{
    display: flex;
    flex-direction: column;
    margin-bottom: 8px;
    border-left: 3px solid #444;
    padding-left: 6px;
  }}
  .row {{
    display: flex;
    height: 22px;
    margin-bottom: 2px;
    overflow: hidden;
  }}
  .block {{
    height: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    padding: 0 6px;
    line-height: 22px;
    font-size: 0.78em;
    cursor: default;
    box-sizing: border-box;
    border-right: 1px solid rgba(0,0,0,0.3);
    min-width: 4px;
  }}
  .block:hover {{ filter: brightness(1.25); }}

  .user           {{ background: #00838f; color: #fff; }}
  .assistant-text {{ background: #2e7d32; color: #fff; }}
  .thinking       {{ background: #4527a0; color: #fff; opacity: 0.8; }}
  .tool-call      {{ background: #f9a825; color: #1a1a1a; }}
  .tool-result    {{ background: #455a64; color: #fff; }}
  .tool-error     {{ background: #c62828; color: #fff; }}
  .compaction     {{ background: #ef6c00; color: #fff; }}
  .meta           {{ background: #424242; color: #fff; font-style: italic; }}

  .turn-header {{
    font-size: 0.78em;
    opacity: 0.6;
    margin-bottom: 2px;
  }}
  .stats {{
    margin-top: 1em;
    font-size: 0.85em;
    opacity: 0.7;
  }}
</style>
</head>
<body>
<h1>pi trajectory · {short_id}</h1>
<div class=\"legend\">
  <span class=\"user\">user</span>
  <span class=\"assistant-text\">assistant</span>
  <span class=\"thinking\">thinking</span>
  <span class=\"tool-call\">tool_call</span>
  <span class=\"tool-result\">tool_result</span>
  <span class=\"tool-error\">tool_error</span>
  <span class=\"compaction\">compaction</span>
</div>
{body}
<div class=\"stats\">
  estimated tokens: {total_int} &middot; entries: {entry_count}
</div>
</body>
</html>
",
        short_id = short_id,
        body = body,
        total_int = total as u64,
        entry_count = branch.len(),
    )
}

fn render_body(branch: &[SessionEntry], total_tokens: f64) -> String {
    // Group into turns: a turn starts at each User entry (or the
    // session start) and runs through the next User entry.
    let mut turns: Vec<Vec<&SessionEntry>> = Vec::new();
    let mut current: Vec<&SessionEntry> = Vec::new();
    for e in branch {
        if matches!(e.kind, SessionEntryKind::User { .. }) && !current.is_empty() {
            turns.push(std::mem::take(&mut current));
        }
        current.push(e);
    }
    if !current.is_empty() {
        turns.push(current);
    }

    let mut html = String::new();
    for (i, turn) in turns.iter().enumerate() {
        html.push_str(&format!(
            "<div class=\"turn\"><div class=\"turn-header\">turn {n}</div>",
            n = i + 1
        ));
        // One row containing every block in this turn, ordered.
        html.push_str("<div class=\"row\">");
        for e in turn {
            if let Some(block) = render_block(e, total_tokens) {
                html.push_str(&block);
            }
        }
        html.push_str("</div></div>");
    }
    html
}

fn render_block(entry: &SessionEntry, total_tokens: f64) -> Option<String> {
    let (class, label, tokens) = match &entry.kind {
        SessionEntryKind::User { message } => {
            let t = est_tokens(&message.text());
            ("user", short(&message.text(), 60), t)
        }
        SessionEntryKind::Assistant { message } => {
            let t = est_tokens(&message.text());
            ("assistant-text", short(&message.text(), 60), t)
        }
        SessionEntryKind::ToolCall { call } => {
            let label = format!(
                "{}({})",
                call.name,
                short(
                    call.input
                        .get("command")
                        .or_else(|| call.input.get("path"))
                        .or_else(|| call.input.get("file_path"))
                        .or_else(|| call.input.get("pattern"))
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                        .as_str(),
                    40,
                )
            );
            let t = est_tokens(&call.input.to_string());
            ("tool-call", label, t)
        }
        SessionEntryKind::ToolResult { result } => {
            let class = if result.is_error {
                "tool-error"
            } else {
                "tool-result"
            };
            let t = est_tokens(&result.model_output);
            (class, short(&result.model_output, 60), t)
        }
        SessionEntryKind::Usage { usage } => {
            // Usage entries don't render their own block — they're
            // already accounted for via the assistant/tool entries
            // they describe. Return None to skip.
            let _ = usage;
            return None;
        }
        SessionEntryKind::Compaction { summary, .. } => {
            ("compaction", short(summary, 60), est_tokens(summary))
        }
        SessionEntryKind::Meta { .. } => ("meta", "session start".into(), 1.0),
        SessionEntryKind::SystemPrompt { text } => {
            ("meta", short(text, 40), est_tokens(text) * 0.5)
        }
        SessionEntryKind::ContextLoad { source, bytes, tokens } => {
            let t = tokens.map(|t| t as f64).unwrap_or(*bytes as f64 / 4.0);
            ("meta", format!("ctx: {source}"), t)
        }
        SessionEntryKind::Outcome { success, score, .. } => {
            let label = format!(
                "outcome: {} ({:.2})",
                if *success { "win" } else { "loss" },
                score.unwrap_or(0.0)
            );
            ("meta", label, 1.0)
        }
        SessionEntryKind::EvolveMarker { generation, .. } => {
            ("meta", format!("evolve gen {generation}"), 1.0)
        }
    };
    let pct = (tokens / total_tokens * 100.0).max(0.05);
    Some(format!(
        "<div class=\"block {class}\" style=\"width: {pct:.3}%\" title=\"{title}\">{label}</div>",
        title = html_escape(&label),
        label = html_escape(&label),
    ))
}

fn total_tokens(branch: &[SessionEntry]) -> u64 {
    let mut from_usage = 0u64;
    for e in branch {
        if let SessionEntryKind::Usage { usage } = &e.kind {
            from_usage += usage.input_tokens as u64 + usage.output_tokens as u64;
        }
    }
    if from_usage > 0 {
        return from_usage;
    }
    // Fall back to char-based estimate.
    let mut total = 0.0f64;
    for e in branch {
        match &e.kind {
            SessionEntryKind::User { message } => total += est_tokens(&message.text()),
            SessionEntryKind::Assistant { message } => total += est_tokens(&message.text()),
            SessionEntryKind::ToolCall { call } => total += est_tokens(&call.input.to_string()),
            SessionEntryKind::ToolResult { result } => total += est_tokens(&result.model_output),
            SessionEntryKind::Compaction { summary, .. } => total += est_tokens(summary),
            SessionEntryKind::SystemPrompt { text } => total += est_tokens(text) * 0.5,
            SessionEntryKind::ContextLoad { tokens, bytes, .. } => {
                total += tokens.map(|t| t as f64).unwrap_or(*bytes as f64 / 4.0)
            }
            _ => {}
        }
    }
    total as u64
}

fn est_tokens(s: &str) -> f64 {
    s.chars().count() as f64 / 4.0
}

fn short(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.replace('\n', " ");
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{}…", head.replace('\n', " "))
}

fn html_escape(s: &str) -> String {
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
