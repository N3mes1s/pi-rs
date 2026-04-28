//! The renderer that turns agent events into a [`pi_tui::Frame`].
//!
//! The interactive mode owns a [`Transcript`], pushes events into it as
//! they stream in, and asks for a `Frame` once per render tick. The
//! renderer is pure — given the same transcript and the same viewport
//! size, it always produces the same frame, which is what makes the
//! diff renderer in `pi-tui` actually useful.

use crossterm::style::Color;
use pi_agent_core::{AgentEvent, AgentEventKind};
use pi_ai::{ContentBlock, Role, Usage};
use pi_tui::{Frame, Line, Span, Theme};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub enum Block {
    User(String),
    AssistantText(String),
    Thinking(String),
    ToolCall {
        name: String,
        input_pretty: String,
    },
    ToolResult {
        ok: bool,
        body: String,
        lines: usize,
    },
    Error(String),
    Compact {
        summary: String,
        freed_tokens: u64,
    },
    Note(String),
}

#[derive(Debug, Default, Clone)]
pub struct Transcript {
    pub blocks: Vec<Block>,
    pub usage_total: Usage,
    pub model_label: String,
    pub queue: Vec<String>,
    pub thinking_collapsed: bool,
    pub tool_collapsed: bool,
}

impl Transcript {
    pub fn push_block(&mut self, b: Block) {
        // Coalesce consecutive AssistantText/Thinking deltas so we don't
        // create one block per token.
        match (self.blocks.last_mut(), &b) {
            (Some(Block::AssistantText(prev)), Block::AssistantText(new)) => prev.push_str(new),
            (Some(Block::Thinking(prev)), Block::Thinking(new)) => prev.push_str(new),
            _ => self.blocks.push(b),
        }
    }

    pub fn ingest(&mut self, ev: &AgentEvent) {
        match &ev.kind {
            AgentEventKind::UserMessage { message } => {
                let mut text = String::new();
                for c in &message.content {
                    if let ContentBlock::Text { text: t } = c {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    self.blocks.push(Block::User(text));
                }
            }
            AgentEventKind::AssistantTextDelta { text } => {
                self.push_block(Block::AssistantText(text.clone()));
            }
            AgentEventKind::AssistantThinkingDelta { text } => {
                self.push_block(Block::Thinking(text.clone()));
            }
            AgentEventKind::AssistantToolCall { call } => {
                self.blocks.push(Block::ToolCall {
                    name: call.name.clone(),
                    input_pretty: serde_json::to_string_pretty(&call.input).unwrap_or_default(),
                });
            }
            AgentEventKind::ToolResult { result } => {
                let lines = result.model_output.lines().count();
                self.blocks.push(Block::ToolResult {
                    ok: !result.is_error,
                    body: result.model_output.clone(),
                    lines,
                });
            }
            AgentEventKind::Usage { usage } => {
                self.usage_total.input_tokens += usage.input_tokens;
                self.usage_total.output_tokens += usage.output_tokens;
                self.usage_total.cache_read_tokens += usage.cache_read_tokens;
                self.usage_total.cache_write_tokens += usage.cache_write_tokens;
                self.usage_total.reasoning_tokens += usage.reasoning_tokens;
                self.usage_total.cost_usd += usage.cost_usd;
            }
            AgentEventKind::Error { message } => {
                self.blocks.push(Block::Error(message.clone()));
            }
            AgentEventKind::CompactionComplete {
                summary,
                freed_tokens,
            } => {
                self.blocks.push(Block::Compact {
                    summary: summary.clone(),
                    freed_tokens: *freed_tokens,
                });
            }
            AgentEventKind::AssistantMessage { message } => {
                // We've already drained text/thinking via deltas; just record
                // tool-use blocks here if we missed them.
                for c in &message.content {
                    if let ContentBlock::ToolUse { name, input, .. } = c {
                        if !self
                            .blocks
                            .iter()
                            .rev()
                            .any(|b| matches!(b, Block::ToolCall { name: n, .. } if n == name))
                        {
                            self.blocks.push(Block::ToolCall {
                                name: name.clone(),
                                input_pretty: serde_json::to_string_pretty(input)
                                    .unwrap_or_default(),
                            });
                        }
                    }
                }
                let _ = message.role; // silence unused
                let _ = Role::User;
            }
            _ => {}
        }
    }

    pub fn render(&self, theme: &Theme, viewport_cols: u16) -> Frame {
        let mut lines: Vec<Line> = Vec::new();
        for b in &self.blocks {
            match b {
                Block::User(t) => render_block(
                    &mut lines,
                    "you",
                    theme.user.to_crossterm(),
                    t,
                    viewport_cols,
                ),
                Block::AssistantText(t) => render_block(
                    &mut lines,
                    "pi",
                    theme.assistant.to_crossterm(),
                    t,
                    viewport_cols,
                ),
                Block::Thinking(t) => {
                    if !self.thinking_collapsed {
                        render_block(
                            &mut lines,
                            "thinking",
                            theme.thinking.to_crossterm(),
                            t,
                            viewport_cols,
                        );
                    } else {
                        lines.push(Line {
                            spans: vec![Span::coloured(
                                format!("[thinking collapsed: {} chars]", t.len()),
                                theme.muted.to_crossterm(),
                            )],
                        });
                    }
                }
                Block::ToolCall { name, input_pretty } => {
                    lines.push(Line {
                        spans: vec![Span::coloured(
                            format!(
                                "→ {} {}",
                                name,
                                input_pretty
                                    .replace('\n', " ")
                                    .chars()
                                    .take(viewport_cols.saturating_sub(8) as usize)
                                    .collect::<String>()
                            ),
                            theme.tool.to_crossterm(),
                        )],
                    });
                }
                Block::ToolResult {
                    ok,
                    body,
                    lines: count,
                } => {
                    let color = if *ok {
                        theme.muted.to_crossterm()
                    } else {
                        theme.error.to_crossterm()
                    };
                    if self.tool_collapsed {
                        lines.push(Line {
                            spans: vec![Span::coloured(
                                format!("  [tool output: {} lines]", count),
                                color,
                            )],
                        });
                    } else {
                        for raw in body.lines().take(20) {
                            for chunk in wrap_line(raw, viewport_cols.saturating_sub(2) as usize) {
                                lines.push(Line {
                                    spans: vec![Span::coloured(format!("  {}", chunk), color)],
                                });
                            }
                        }
                        if *count > 20 {
                            lines.push(Line {
                                spans: vec![Span::coloured(
                                    format!("  … (+{} lines)", *count - 20),
                                    color,
                                )],
                            });
                        }
                    }
                }
                Block::Error(m) => lines.push(Line {
                    spans: vec![Span::coloured(
                        format!("[error] {}", m),
                        theme.error.to_crossterm(),
                    )],
                }),
                Block::Compact {
                    summary,
                    freed_tokens,
                } => lines.push(Line {
                    spans: vec![Span::coloured(
                        format!(
                            "[compacted ~{} tokens] {}",
                            freed_tokens,
                            summary
                                .replace('\n', " ")
                                .chars()
                                .take(120)
                                .collect::<String>()
                        ),
                        theme.muted.to_crossterm(),
                    )],
                }),
                Block::Note(m) => {
                    // Note bodies are multi-line strings (e.g. /help output).
                    // The differential renderer expects one logical line per
                    // `Line`; embedding `\n` inside a single Line cascades
                    // diagonally because raw-mode output doesn't reset the
                    // cursor column on bare LF.
                    for piece in m.split('\n') {
                        lines.push(Line {
                            spans: vec![Span::coloured(
                                piece.to_string(),
                                theme.muted.to_crossterm(),
                            )],
                        });
                    }
                }
            }
        }
        // Separator before the input area.
        lines.push(Line::default());
        Frame { lines, cursor_at: None }
    }

    pub fn footer(&self, theme: &Theme, model: &str, cwd: &std::path::Path) -> Line {
        Line {
            spans: vec![
                Span::coloured(format!("{}  ", model), theme.accent.to_crossterm()),
                Span::coloured(
                    format!(
                        "in:{} out:{} ${:.4}",
                        self.usage_total.input_tokens,
                        self.usage_total.output_tokens,
                        self.usage_total.cost_usd
                    ),
                    theme.muted.to_crossterm(),
                ),
                Span::coloured(
                    format!("  cwd:{}", cwd.display()),
                    theme.muted.to_crossterm(),
                ),
            ],
        }
    }

    /// Powerline-style footer used by the interactive TUI. Format:
    ///
    /// ```text
    ///  model ▶ cwd ▶ git: branch ●S+M ▶ in:N out:N $X.XXXX ▶ ctx:N%
    /// ```
    ///
    /// Sections that don't apply (no git repo, no `context_window`)
    /// are skipped. The ▶ separator is rendered in the muted theme
    /// colour; segment text uses the accent / muted palette.
    pub fn footer_powerline(
        &self,
        theme: &Theme,
        model: &str,
        cwd: &std::path::Path,
        git: Option<&crate::footer::GitStatus>,
        context_window: Option<u32>,
    ) -> Line {
        let muted = theme.muted.to_crossterm();
        let accent = theme.accent.to_crossterm();
        let mut spans: Vec<Span> = Vec::new();
        // leading space so the first arrow doesn't kiss the edge
        spans.push(Span::plain(" ".to_string()));

        let push_sep = |spans: &mut Vec<Span>| {
            spans.push(Span::coloured(" ▶ ".to_string(), muted));
        };

        // 1. model
        spans.push(Span::coloured(model.to_string(), accent));

        // 2. cwd (compact: replace $HOME with ~)
        let cwd_display = compact_cwd(cwd);
        push_sep(&mut spans);
        spans.push(Span::coloured(cwd_display, muted));

        // 3. git
        if let Some(g) = git {
            push_sep(&mut spans);
            spans.push(Span::coloured(crate::footer::format_git(g), muted));
        }

        // 4. usage
        push_sep(&mut spans);
        spans.push(Span::coloured(
            format!(
                "in:{} out:{} ${:.4}",
                self.usage_total.input_tokens,
                self.usage_total.output_tokens,
                self.usage_total.cost_usd,
            ),
            muted,
        ));

        // 5. ctx percentage (input-tokens / context_window)
        if let Some(cw) = context_window {
            if cw > 0 {
                let pct = (self.usage_total.input_tokens as f64 / cw as f64) * 100.0;
                let pct = pct.clamp(0.0, 100.0);
                push_sep(&mut spans);
                spans.push(Span::coloured(format!("ctx:{:.0}%", pct), muted));
            }
        }

        // trailing space so the closing edge breathes
        spans.push(Span::plain(" ".to_string()));
        Line { spans }
    }

    pub fn tail(&self, n: usize) -> &[Block] {
        let len = self.blocks.len();
        let start = len.saturating_sub(n);
        &self.blocks[start..]
    }
}

fn render_block(lines: &mut Vec<Line>, label: &str, color: Color, body: &str, cols: u16) {
    let max = cols.saturating_sub(label.len() as u16 + 3) as usize;
    let max = max.max(10);
    let mut first = true;
    for raw in body.lines() {
        for chunk in wrap_line(raw, max) {
            if first {
                lines.push(Line {
                    spans: vec![
                        Span::coloured(format!("{}> ", label), color),
                        Span::plain(chunk.to_string()),
                    ],
                });
                first = false;
            } else {
                lines.push(Line {
                    spans: vec![
                        Span::coloured(" ".repeat(label.len() + 2), color),
                        Span::plain(chunk.to_string()),
                    ],
                });
            }
        }
        if first {
            lines.push(Line {
                spans: vec![Span::coloured(format!("{}> ", label), color)],
            });
            first = false;
        }
    }
}

fn wrap_line(s: &str, width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for g in s.graphemes(true) {
        let gw = UnicodeWidthStr::width(g);
        if current_w + gw > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_w = 0;
        }
        current.push_str(g);
        current_w += gw;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Replace the user's `$HOME` with `~` for a tidier footer. If `cwd`
/// can't be made relative to home (e.g. /tmp/foo) the absolute path
/// is returned unchanged.
fn compact_cwd(cwd: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = cwd.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    cwd.display().to_string()
}
