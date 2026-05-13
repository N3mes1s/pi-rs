//! The renderer that turns agent events into a [`pi_tui::Frame`].
//!
//! The interactive mode owns a [`Transcript`], pushes events into it as
//! they stream in, and asks for a `Frame` once per render tick. The
//! renderer is pure — given the same transcript and the same viewport
//! size, it always produces the same frame, which is what makes the
//! diff renderer in `pi-tui` actually useful.

use crossterm::style::Color;
use pi_agent_core::{AgentEvent, AgentEventKind, RouteMode};
use pi_ai::{ContentBlock, Role, Usage};
use pi_tui::{Frame, Line, Span, Theme};

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
        // Welcome banner is ALWAYS the first transcript prelude — it
        // sits above the first user message and scrolls up with the
        // conversation just like terminal MOTD output. Earlier
        // behaviour gated this on `self.blocks.is_empty()` which made
        // the logo disappear in a jarring jump on the first message;
        // with the banner as a permanent prelude there's no jump and
        // the banner naturally scrolls out of frame once the
        // transcript grows past `max_transcript` rows in build_frame.
        push_welcome_banner(&mut lines, theme, viewport_cols, &self.model_label);
        for b in &self.blocks {
            match b {
                Block::User(t) => render_block(
                    &mut lines,
                    "you",
                    theme.user.to_crossterm(),
                    t,
                    viewport_cols,
                ),
                Block::AssistantText(t) => {
                    // Use markdown rendering for assistant text
                    let md_lines = crate::markdown::parse_and_render_markdown(
                        t,
                        theme.accent.to_crossterm(),
                        theme.muted.to_crossterm(),
                        viewport_cols.saturating_sub(4),
                    );
                    let mut first = true;
                    for line in md_lines {
                        if first {
                            // Prefix first line with "pi>"
                            let mut prefixed_spans = vec![Span::coloured(
                                "pi> ".to_string(),
                                theme.assistant.to_crossterm(),
                            )];
                            prefixed_spans.extend(line.spans);
                            lines.push(Line {
                                spans: prefixed_spans,
                            });
                            first = false;
                        } else {
                            // Continuation lines with padding
                            let mut padded_spans = vec![Span::coloured(
                                "    ".to_string(),
                                theme.assistant.to_crossterm(),
                            )];
                            padded_spans.extend(line.spans);
                            lines.push(Line {
                                spans: padded_spans,
                            });
                        }
                    }
                }
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
                Block::Error(m) => {
                    // Multi-line errors (stack traces, formatted
                    // messages) must split into one Line per logical
                    // newline — pi-tui's renderer hard-wraps on cell
                    // width but doesn't honour '\n' inside a span's
                    // text, so a flat `[error] a\nb` would render as
                    // a single glitched row.
                    for (i, ln) in m.split('\n').enumerate() {
                        let text = if i == 0 {
                            format!("[error] {ln}")
                        } else {
                            format!("        {ln}")
                        };
                        lines.push(Line {
                            spans: vec![Span::coloured(text, theme.error.to_crossterm())],
                        });
                    }
                }
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
        Frame {
            lines,
            cursor_at: None,
            scroll_offset: 0,
        }
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

    /// Powerline-style footer used by the interactive TUI when the terminal
    /// advertises 256 colours. Format:
    ///
    /// ```text
    ///  model ▶ cwd ▶ git: branch ●S+M ▶ route:auto ▶ $X.XXXX ▶ ctx:N%
    /// ```
    ///
    /// When 256-colour support is unavailable the function degrades to the
    /// legacy dim-line footer instead of emitting styled background segments.
    pub fn footer_powerline(
        &self,
        theme: &Theme,
        model: &str,
        cwd: &std::path::Path,
        git: Option<&crate::footer::GitStatus>,
        route_mode: RouteMode,
        context_window: Option<u32>,
        available_colors: Option<u16>,
    ) -> Line {
        if available_colors.unwrap_or_else(crossterm::style::available_color_count) < 256 {
            return self.footer_powerline_fallback(
                theme,
                model,
                cwd,
                git,
                route_mode,
                context_window,
            );
        }

        let mut spans: Vec<Span> = vec![Span::plain(" ")];
        let text_fg = theme.fg.to_crossterm();
        let muted_fg = theme.muted.to_crossterm();
        let accent_fg = theme.accent.to_crossterm();
        let segment_bgs = [
            theme.accent.to_crossterm(),
            theme.user.to_crossterm(),
            theme.tool.to_crossterm(),
            theme.assistant.to_crossterm(),
            theme.error.to_crossterm(),
            theme.muted.to_crossterm(),
        ];
        let divider_fg = theme.bg.to_crossterm();

        let mut segment_index = 0usize;
        let push_segment =
            |spans: &mut Vec<Span>, label: String, fg: Color, bg: Color, next_bg: Option<Color>| {
                spans.push(Span::styled(format!(" {label} "), fg, bg));
                match next_bg {
                    Some(next) => spans.push(Span::styled("", bg, next)),
                    None => spans.push(Span::styled("", bg, divider_fg)),
                }
            };

        let cwd_display = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| compact_cwd(cwd));
        let mut segments: Vec<(String, Color)> =
            vec![(model.to_string(), text_fg), (cwd_display, accent_fg)];
        if let Some(g) = git {
            segments.push((crate::footer::format_git(g), text_fg));
        }
        segments.push((format!("route:{}", route_mode_label(route_mode)), text_fg));
        segments.push((format!("${:.4}", self.usage_total.cost_usd), text_fg));
        if let Some(cw) = context_window {
            if cw > 0 {
                let pct = (self.usage_total.input_tokens as f64 / cw as f64) * 100.0;
                segments.push((format!("ctx:{:.0}%", pct.clamp(0.0, 100.0)), muted_fg));
            }
        }

        for (idx, (label, fg)) in segments.iter().enumerate() {
            let bg = segment_bgs[segment_index % segment_bgs.len()];
            let next_bg = segments
                .get(idx + 1)
                .map(|_| segment_bgs[(segment_index + 1) % segment_bgs.len()]);
            push_segment(&mut spans, label.clone(), *fg, bg, next_bg);
            segment_index += 1;
        }

        Line { spans }
    }

    fn footer_powerline_fallback(
        &self,
        theme: &Theme,
        model: &str,
        cwd: &std::path::Path,
        git: Option<&crate::footer::GitStatus>,
        route_mode: RouteMode,
        context_window: Option<u32>,
    ) -> Line {
        let muted = theme.muted.to_crossterm();
        let accent = theme.accent.to_crossterm();
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::plain(" ".to_string()));

        let push_sep = |spans: &mut Vec<Span>| {
            spans.push(Span::coloured(" ▶ ".to_string(), muted));
        };

        spans.push(Span::coloured(model.to_string(), accent));
        push_sep(&mut spans);
        spans.push(Span::coloured(compact_cwd(cwd), muted));
        if let Some(g) = git {
            push_sep(&mut spans);
            spans.push(Span::coloured(crate::footer::format_git(g), muted));
        }
        push_sep(&mut spans);
        spans.push(Span::coloured(
            format!("route:{}", route_mode_label(route_mode)),
            muted,
        ));
        push_sep(&mut spans);
        spans.push(Span::coloured(
            format!("${:.4}", self.usage_total.cost_usd),
            muted,
        ));
        if let Some(cw) = context_window {
            if cw > 0 {
                let pct = (self.usage_total.input_tokens as f64 / cw as f64) * 100.0;
                push_sep(&mut spans);
                spans.push(Span::coloured(
                    format!("ctx:{:.0}%", pct.clamp(0.0, 100.0)),
                    muted,
                ));
            }
        }
        spans.push(Span::plain(" ".to_string()));
        Line { spans }
    }

    pub fn tail(&self, n: usize) -> &[Block] {
        let len = self.blocks.len();
        let start = len.saturating_sub(n);
        &self.blocks[start..]
    }
}

/// Push a startup welcome banner: a five-row "Pi" block-glyph logo in
/// rust-iron-oxide colours (gradient top→bottom from bright copper to
/// dark patina) plus a brief tip line. Inspired by oh-my-pi's welcome
/// component but recoloured for Rust and trimmed to a single column —
/// pi-rs renders the model + cost in the powerline footer already.
fn push_welcome_banner(lines: &mut Vec<Line>, theme: &Theme, cols: u16, model_label: &str) {
    // Block-glyph "Pi" logo (port of oh-my-pi's piLogo).
    let logo: [&str; 5] = [
        "▀████████████▀",
        " ╘███    ███  ",
        "  ███    ███  ",
        "  ███    ███  ",
        " ▄███▄  ▄███▄ ",
    ];
    // Iron-oxide / rust-patina gradient. Top row is bright "fresh rust",
    // bottom row is dark patina — like the colour shift on weathered
    // steel.
    let rust_palette: [Color; 5] = [
        Color::Rgb {
            r: 0xe8,
            g: 0x88,
            b: 0x4d,
        }, // bright copper
        Color::Rgb {
            r: 0xd9,
            g: 0x6b,
            b: 0x3a,
        }, // amber rust
        Color::Rgb {
            r: 0xce,
            g: 0x42,
            b: 0x2b,
        }, // Rust language brand
        Color::Rgb {
            r: 0xa7,
            g: 0x36,
            b: 0x1d,
        }, // oxidised iron
        Color::Rgb {
            r: 0x7a,
            g: 0x28,
            b: 0x12,
        }, // dark patina
    ];
    let logo_width = 14u16; // visible width of each row above
    let pad = ((cols.saturating_sub(logo_width)) / 2) as usize;
    let pad_str = " ".repeat(pad);
    lines.push(Line::default());
    for (row, glyph) in logo.iter().enumerate() {
        lines.push(Line {
            spans: vec![
                Span::plain(pad_str.clone()),
                Span::coloured((*glyph).to_string(), rust_palette[row]),
            ],
        });
    }
    lines.push(Line::default());
    let title = "pi-rs — agentic coding in Rust";
    let title_pad = ((cols as usize).saturating_sub(title.len())) / 2;
    lines.push(Line {
        spans: vec![
            Span::plain(" ".repeat(title_pad)),
            Span::coloured(
                title.to_string(),
                Color::Rgb {
                    r: 0xce,
                    g: 0x42,
                    b: 0x2b,
                },
            ),
        ],
    });
    // Tip line: each trigger glyph gets its own colour so the eye
    // can scan and the muted instruction text falls away. Layout:
    //   /help  ·  @files  ·  !shell  ·  /quit
    // Bright trigger ◆ then muted explanation, separator dots in
    // the dimmest tone.
    let muted = theme.muted.to_crossterm();
    let dim_dot = Color::Rgb {
        r: 0x4a,
        g: 0x4a,
        b: 0x4a,
    };
    let slash_c = Color::Rgb {
        r: 0xce,
        g: 0x42,
        b: 0x2b,
    }; // Rust orange
    let at_c = Color::Rgb {
        r: 0x6c,
        g: 0xa0,
        b: 0xdc,
    }; // user blue
    let bang_c = Color::Rgb {
        r: 0xc4,
        g: 0xa6,
        b: 0x4d,
    }; // tool yellow
    let quit_c = Color::Rgb {
        r: 0x9a,
        g: 0x4a,
        b: 0x4a,
    }; // dim red
    let make_tip = |spans: &mut Vec<Span>| {
        spans.push(Span::coloured("/help".to_string(), slash_c));
        spans.push(Span::coloured(" commands  ".to_string(), muted));
        spans.push(Span::coloured("·  ".to_string(), dim_dot));
        spans.push(Span::coloured("@".to_string(), at_c));
        spans.push(Span::coloured("files  ".to_string(), muted));
        spans.push(Span::coloured("·  ".to_string(), dim_dot));
        spans.push(Span::coloured("!".to_string(), bang_c));
        spans.push(Span::coloured("shell  ".to_string(), muted));
        spans.push(Span::coloured("·  ".to_string(), dim_dot));
        spans.push(Span::coloured("/quit".to_string(), quit_c));
        spans.push(Span::coloured(" to exit".to_string(), muted));
    };
    // Build to a temp Vec to compute the visible width for centering.
    // Skip the tip on viewports too narrow to fit it without overflow —
    // a wrapped tip line garbles into nonsense and a clipped one is a
    // worse UX than no tip at all (the keys are also documented in
    // /help itself).
    let tip_visible = "/help commands  ·  @files  ·  !shell  ·  /quit to exit";
    if (cols as usize) >= tip_visible.len() {
        let tip_pad = ((cols as usize) - tip_visible.len()) / 2;
        let mut tip_spans = vec![Span::plain(" ".repeat(tip_pad))];
        make_tip(&mut tip_spans);
        lines.push(Line { spans: tip_spans });
    }
    if !model_label.is_empty() {
        let model_line = format!("connected to {}", model_label);
        let pad = ((cols as usize).saturating_sub(model_line.len())) / 2;
        lines.push(Line {
            spans: vec![
                Span::plain(" ".repeat(pad)),
                Span::coloured("connected to ".to_string(), muted),
                Span::coloured(
                    model_label.to_string(),
                    Color::Rgb {
                        r: 0xe8,
                        g: 0x88,
                        b: 0x4d,
                    },
                ),
            ],
        });
    }
    lines.push(Line::default());
}

fn render_block(lines: &mut Vec<Line>, label: &str, color: Color, body: &str, cols: u16) {
    let prefix_width = label.len() as u16 + 2;
    let max = cols.saturating_sub(prefix_width) as usize;
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

pub fn wrap_line_pub(s: &str, width: usize) -> Vec<String> {
    wrap_line(s, width)
}

fn wrap_line(s: &str, width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return vec![s.to_string()];
    }
    // Delegate to the textwrap crate for Unicode-aware word-boundary wrapping.
    // This is the canonical wrapping path per RFD 0024: textwrap handles
    // grapheme-aware word breaking via UnicodeBreakProperties, and
    // HyphenSplitter allows breaks at hyphens within very long words.
    let options = textwrap::Options::new(width)
        .word_separator(textwrap::WordSeparator::UnicodeBreakProperties)
        .word_splitter(textwrap::WordSplitter::HyphenSplitter);
    textwrap::wrap(s, &options)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect()
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

fn route_mode_label(mode: RouteMode) -> &'static str {
    match mode {
        RouteMode::Off => "off",
        RouteMode::Static => "static",
        RouteMode::Auto => "auto",
        RouteMode::Learned => "learned",
    }
}
