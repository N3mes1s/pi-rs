//! Trajectory flamegraph renderer (G11) + agent-readable JSON view (RFD 0012).
//!
//! Renders a session branch as either:
//! - a self-contained HTML page where width is proportional to estimated
//!   token cost and depth is the natural turn nesting (the original G11
//!   view; HTML output is byte-for-byte identical to pre-RFD-0012); or
//! - a JSON document with `{ session_id, estimated_tokens, turns: [...] }`
//!   that downstream agents (the evolve daemon, future analyzers) can
//!   ingest without parsing HTML.
//!
//! The two views share a [`Trajectory`] model built once from the branch.

use pi_agent_core::{SessionEntry, SessionEntryKind};
use serde::Serialize;

/// Output format for `pi --flamegraph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Html,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "html" => Some(Format::Html),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

// ─── shared model ───────────────────────────────────────────────────────

/// Agent-readable trajectory shape. The HTML and JSON renderers both
/// derive from this; the JSON renderer is just `serde_json::to_string_pretty`.
#[derive(Debug, Clone, Serialize)]
pub struct Trajectory {
    pub session_id: String,
    pub estimated_tokens: u64,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub index: usize,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub kind: String,
    pub tokens: u64,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutcomeData {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

// ─── public render entry points ─────────────────────────────────────────

/// Backwards-compatible HTML render. Preserved byte-for-byte from the
/// pre-RFD-0012 implementation so existing fixtures keep matching.
pub fn render(session_id: &str, branch: &[SessionEntry]) -> String {
    render_html_branch(session_id, branch)
}

/// Render with an explicit format selector.
pub fn render_with_format(session_id: &str, branch: &[SessionEntry], format: Format) -> String {
    match format {
        Format::Html => render_html_branch(session_id, branch),
        Format::Json => {
            let trajectory = build_trajectory(session_id, branch);
            render_json(&trajectory)
        }
    }
}

/// Render a [`Trajectory`] as agent-readable JSON.
pub fn render_json(trajectory: &Trajectory) -> String {
    serde_json::to_string_pretty(trajectory).unwrap_or_else(|_| "{}".into())
}

/// Render the trajectory of `branch` as the original self-contained
/// HTML document. The output is unchanged from pre-RFD-0012.
pub fn render_html(trajectory: &Trajectory, branch: &[SessionEntry]) -> String {
    render_html_inner(&trajectory.session_id, branch)
}

// ─── trajectory builder ─────────────────────────────────────────────────

/// Build the shared [`Trajectory`] model from a branch.
pub fn build_trajectory(session_id: &str, branch: &[SessionEntry]) -> Trajectory {
    let estimated_tokens = total_tokens(branch);
    let turns_raw = group_turns(branch);
    let mut turns = Vec::with_capacity(turns_raw.len());
    for (i, turn) in turns_raw.iter().enumerate() {
        let mut blocks: Vec<Block> = Vec::new();
        // Locate any Usage entry in this turn so we can attach
        // cost_usd to the assistant_text block.
        let mut turn_cost: Option<f64> = None;
        for e in turn {
            if let SessionEntryKind::Usage { usage } = &e.kind {
                if usage.cost_usd > 0.0 {
                    turn_cost = Some(usage.cost_usd);
                }
            }
        }
        for e in turn {
            if let Some(mut b) = entry_as_block(e) {
                if b.kind == "assistant_text" && b.cost_usd.is_none() {
                    b.cost_usd = turn_cost;
                }
                blocks.push(b);
            }
        }
        turns.push(Turn {
            index: i + 1,
            blocks,
        });
    }
    Trajectory {
        session_id: session_id.to_string(),
        estimated_tokens,
        turns,
    }
}

fn group_turns(branch: &[SessionEntry]) -> Vec<Vec<&SessionEntry>> {
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
    turns
}

fn entry_as_block(entry: &SessionEntry) -> Option<Block> {
    let (kind, label, tokens, outcome) = match &entry.kind {
        SessionEntryKind::User { message } => {
            let text = message.text();
            ("user", short(&text, 60), est_tokens(&text) as u64, None)
        }
        SessionEntryKind::Assistant { message } => {
            let text = message.text();
            (
                "assistant_text",
                short(&text, 60),
                est_tokens(&text) as u64,
                None,
            )
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
            let t = est_tokens(&call.input.to_string()) as u64;
            ("tool_call", label, t, None)
        }
        SessionEntryKind::ToolResult { result } => {
            let kind = if result.is_error {
                "tool_error"
            } else {
                "tool_result"
            };
            (
                kind,
                short(&result.model_output, 60),
                est_tokens(&result.model_output) as u64,
                None,
            )
        }
        SessionEntryKind::Usage { .. } => return None,
        SessionEntryKind::Compaction { summary, .. } => (
            "compaction",
            short(summary, 60),
            est_tokens(summary) as u64,
            None,
        ),
        SessionEntryKind::Meta { .. } => ("meta", "session start".into(), 0, None),
        SessionEntryKind::SystemPrompt { text } => {
            ("meta", short(text, 40), (est_tokens(text) * 0.5) as u64, None)
        }
        SessionEntryKind::ContextLoad {
            source,
            bytes,
            tokens,
        } => {
            let t = tokens.unwrap_or(*bytes / 4);
            ("context_load", source.clone(), t, None)
        }
        SessionEntryKind::Outcome { success, score, .. } => {
            let label = format!(
                "outcome: {} ({:.2})",
                if *success { "win" } else { "loss" },
                score.unwrap_or(0.0)
            );
            (
                "outcome",
                label,
                0,
                Some(OutcomeData {
                    success: *success,
                    score: *score,
                }),
            )
        }
        SessionEntryKind::EvolveMarker { generation, .. } => {
            ("meta", format!("evolve gen {generation}"), 0, None)
        }
    };
    Some(Block {
        kind: kind.into(),
        tokens,
        label,
        outcome,
        cost_usd: None,
    })
}

// ─── HTML render (byte-for-byte identical to the pre-RFD-0012 view) ─────

fn render_html_branch(session_id: &str, branch: &[SessionEntry]) -> String {
    render_html_inner(session_id, branch)
}

fn render_html_inner(session_id: &str, branch: &[SessionEntry]) -> String {
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
    let turns = group_turns(branch);

    let mut html = String::new();
    for (i, turn) in turns.iter().enumerate() {
        html.push_str(&format!(
            "<div class=\"turn\"><div class=\"turn-header\">turn {n}</div>",
            n = i + 1
        ));
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
    pi_ai::tokenizer::count_default(s) as f64
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
