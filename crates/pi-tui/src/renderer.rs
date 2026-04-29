use crossterm::style::Color;
use ratatui::backend::CrosstermBackend;
use ratatui::style::Style;
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanStyle {
    pub fg: Color,
    pub bg: Color,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub color: Option<Color>,
    pub style: Option<SpanStyle>,
}

impl Span {
    pub fn plain(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            color: None,
            style: None,
        }
    }

    pub fn coloured(s: impl Into<String>, c: Color) -> Self {
        Self {
            text: s.into(),
            color: Some(c),
            style: None,
        }
    }

    pub fn styled(s: impl Into<String>, fg: Color, bg: Color) -> Self {
        Self {
            text: s.into(),
            color: Some(fg),
            style: Some(SpanStyle { fg, bg }),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn plain(s: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::plain(s)],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub lines: Vec<Line>,
    /// Optional hardware-cursor target: `(line_index_in_frame, col)`.
    /// When `Some`, the renderer positions the OS cursor here after
    /// painting and sets it visible. When `None`, the cursor stays
    /// hidden (default for picker overlays etc.).
    pub cursor_at: Option<(u16, u16)>,
}

/// Differential renderer implemented on top of ratatui's terminal backend.
///
/// Public API intentionally stays stable so `pi-coding-agent` can keep using
/// `Frame`, `Line`, and `Span` unchanged while the renderer internals migrate
/// to ratatui.
pub struct DiffRenderer<W: Write> {
    terminal: Terminal<CrosstermBackend<W>>,
    sync: bool,
}

impl<W: Write> DiffRenderer<W> {
    pub fn new(out: W) -> Self {
        let backend = CrosstermBackend::new(out);
        let mut terminal =
            Terminal::new(backend).expect("failed to create ratatui terminal backend");
        terminal.clear().expect("failed to clear ratatui terminal");
        Self {
            terminal,
            sync: std::env::var("PI_NO_SYNC").is_err(),
        }
    }

    pub fn resize(&mut self, _cols: u16) {
        // Ratatui tracks terminal size internally. Clear to force a full repaint
        // after width changes invalidate any upstream wrapping decisions.
        let _ = self.terminal.clear();
    }

    pub fn render(&mut self, frame: &Frame) -> std::io::Result<()> {
        if self.sync {
            self.terminal.backend_mut().write_all(b"\x1b[?2026h")?;
        }

        self.terminal.draw(|f| {
            let area = f.area();
            let rendered_lines = frame
                .lines
                .iter()
                .take(area.height as usize)
                .map(|line| {
                    let spans = line
                        .spans
                        .iter()
                        .map(|span| {
                            let style = if let Some(style) = span.style {
                                Style::default()
                                    .fg(to_ratatui_color(style.fg))
                                    .bg(to_ratatui_color(style.bg))
                            } else {
                                match span.color {
                                    Some(color) => Style::default().fg(to_ratatui_color(color)),
                                    None => Style::default(),
                                }
                            };
                            RtSpan::styled(span.text.clone(), style)
                        })
                        .collect::<Vec<_>>();
                    RtLine::from(spans)
                })
                .collect::<Vec<_>>();

            let text = Text::from(rendered_lines);
            let paragraph = Paragraph::new(text);
            f.render_widget(paragraph, area);

            if let Some((target_line, target_col)) = frame.cursor_at {
                let line = target_line.min(area.height.saturating_sub(1));
                let col = target_col.min(area.width.saturating_sub(1));
                f.set_cursor_position((col, line));
            }
        })?;

        if frame.cursor_at.is_some() {
            self.terminal.show_cursor()?;
        } else {
            self.terminal.hide_cursor()?;
        }

        if self.sync {
            self.terminal.backend_mut().write_all(b"\x1b[?2026l")?;
        }
        self.terminal.backend_mut().flush()?;
        Ok(())
    }
}

fn to_ratatui_color(c: Color) -> ratatui::style::Color {
    match c {
        Color::Reset => ratatui::style::Color::Reset,
        Color::Black => ratatui::style::Color::Black,
        Color::DarkGrey => ratatui::style::Color::DarkGray,
        Color::Red => ratatui::style::Color::Red,
        Color::DarkRed => ratatui::style::Color::Red, // Map dark variants to their light equivalents
        Color::Green => ratatui::style::Color::Green,
        Color::DarkGreen => ratatui::style::Color::Green,
        Color::Yellow => ratatui::style::Color::Yellow,
        Color::DarkYellow => ratatui::style::Color::Yellow,
        Color::Blue => ratatui::style::Color::Blue,
        Color::DarkBlue => ratatui::style::Color::Blue,
        Color::Magenta => ratatui::style::Color::Magenta,
        Color::DarkMagenta => ratatui::style::Color::Magenta,
        Color::Cyan => ratatui::style::Color::Cyan,
        Color::DarkCyan => ratatui::style::Color::Cyan,
        Color::White => ratatui::style::Color::White,
        Color::Grey => ratatui::style::Color::Gray,
        Color::Rgb { r, g, b } => ratatui::style::Color::Rgb(r, g, b),
        Color::AnsiValue(v) => ratatui::style::Color::Indexed(v),
    }
}
