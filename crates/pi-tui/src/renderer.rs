use crossterm::style::Color;
use ratatui::backend::CrosstermBackend;
use ratatui::style::Style;
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io::Write;
use unicode_width::UnicodeWidthChar;

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
            let max_w = area.width.max(1) as usize;

            // Pass 1: hard-wrap each logical Line into one-or-more visual
            // RtLines so content wider than the terminal is still visible,
            // not silently truncated. If the original frame asks for a
            // hardware cursor, walk the same wrap and re-emit the cursor
            // in (visual_row, visual_col) coordinates.
            let mut visual: Vec<RtLine<'static>> = Vec::with_capacity(frame.lines.len());
            let mut visual_cursor: Option<(u16, u16)> = None;
            let cursor_target = frame.cursor_at;

            for (logical_idx, line) in frame.lines.iter().enumerate() {
                let line_cursor_col = cursor_target.and_then(|(lr, lc)| {
                    if lr as usize == logical_idx {
                        Some(lc as usize)
                    } else {
                        None
                    }
                });
                let SplitLine {
                    lines,
                    cursor_offset,
                } = split_line_to_visual(line, max_w, line_cursor_col);
                if let Some((sub, col)) = cursor_offset {
                    let row = visual.len() + sub;
                    visual_cursor = Some((row as u16, col as u16));
                }
                visual.extend(lines);
            }

            // Pass 2: trim from the TOP so the most-recent content
            // (transcript tail + input field + autocomplete dropdown) stays
            // visible. The previous `take(area.height)` from the top
            // dropped the input itself off-screen on tall histories.
            let height = area.height as usize;
            let drop = visual.len().saturating_sub(height);
            if drop > 0 {
                // Move cursor target up by however many lines we just shaved.
                visual_cursor = visual_cursor.map(|(r, c)| (r.saturating_sub(drop as u16), c));
                let _ = visual.drain(..drop);
            }

            let text = Text::from(visual);
            let paragraph = Paragraph::new(text);
            f.render_widget(paragraph, area);

            if let Some((target_line, target_col)) = visual_cursor {
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

/// Result of laying one logical [`Line`] out into visual rows.
///
/// `lines` is the produced sequence of ratatui [`RtLine`]s (one per
/// visual row). `cursor_offset` is `Some((sub, col))` when the caller
/// asked us to track a cursor column on this logical line — `sub` is
/// the offset *within* `lines` (0 = first visual row), `col` is the
/// cell column. Returns `None` when no cursor was requested or the
/// requested cell index lies past the end of the line.
struct SplitLine {
    lines: Vec<RtLine<'static>>,
    cursor_offset: Option<(usize, usize)>,
}

/// Hard-wrap one logical `Line` so no visual row exceeds `max_w` cells.
///
/// We walk character-by-character (Unicode `char`, not byte / grapheme
/// cluster — combining marks are width 0 and ride along with the base
/// char; emoji + CJK take 2 cells via `UnicodeWidthChar`). When the
/// current visual row would overflow, we flush the in-progress span,
/// start a fresh visual row, and continue. Spans crossing a wrap
/// boundary are split into two `RtSpan`s sharing the same style — no
/// styles are lost in the wrap.
///
/// If `cursor_col` is set, we record (sub_row, col) the first time the
/// running cell counter equals it. Cursors past end-of-line clamp to
/// the last visual row's tail (one past the last cell, or column 0 of
/// a new empty row if the line ends exactly at `max_w`).
fn split_line_to_visual(
    line: &Line,
    max_w: usize,
    cursor_col: Option<usize>,
) -> SplitLine {
    let max_w = max_w.max(1);
    let mut rows: Vec<Vec<RtSpan<'static>>> = vec![Vec::new()];
    let mut cur_w: usize = 0;
    let mut total_w: usize = 0;
    let mut cursor_out: Option<(usize, usize)> = None;

    for span in &line.spans {
        let style = if let Some(s) = span.style {
            Style::default()
                .fg(to_ratatui_color(s.fg))
                .bg(to_ratatui_color(s.bg))
        } else if let Some(c) = span.color {
            Style::default().fg(to_ratatui_color(c))
        } else {
            Style::default()
        };

        let mut buf = String::new();
        for ch in span.text.chars() {
            // Record cursor *before* placing this char — the cursor sits to
            // the LEFT of the cell at `total_w`.
            if cursor_col == Some(total_w) && cursor_out.is_none() {
                cursor_out = Some((rows.len() - 1, cur_w));
            }
            let ch_w = ch.width().unwrap_or(0);
            if cur_w + ch_w > max_w {
                if !buf.is_empty() {
                    rows.last_mut().unwrap().push(RtSpan::styled(
                        std::mem::take(&mut buf),
                        style,
                    ));
                }
                rows.push(Vec::new());
                cur_w = 0;
                // The cursor we just recorded (above) was for the OLD row
                // at column `max_w`-ish — if it was a "past the end"
                // cursor we'll fix that below in the end-of-line check.
            }
            buf.push(ch);
            cur_w += ch_w;
            total_w += ch_w;
        }
        if !buf.is_empty() {
            rows.last_mut().unwrap().push(RtSpan::styled(buf, style));
        }
    }

    // End-of-line cursor: caret sitting after the last cell.
    if cursor_col == Some(total_w) && cursor_out.is_none() {
        if cur_w >= max_w {
            rows.push(Vec::new());
            cursor_out = Some((rows.len() - 1, 0));
        } else {
            cursor_out = Some((rows.len() - 1, cur_w));
        }
    }

    SplitLine {
        lines: rows.into_iter().map(RtLine::from).collect(),
        cursor_offset: cursor_out,
    }
}
