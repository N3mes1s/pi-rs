use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::{cursor, queue, terminal};
use std::io::Write;

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub color: Option<Color>,
}

impl Span {
    pub fn plain(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            color: None,
        }
    }

    pub fn coloured(s: impl Into<String>, c: Color) -> Self {
        Self {
            text: s.into(),
            color: Some(c),
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

/// Differential renderer — keeps the previous frame, only redraws lines that
/// differ. Wraps each render in synchronized output where supported.
pub struct DiffRenderer<W: Write> {
    out: W,
    last: Vec<String>,
    width: u16,
    sync: bool,
}

impl<W: Write> DiffRenderer<W> {
    pub fn new(out: W) -> Self {
        let (cols, _) = terminal::size().unwrap_or((80, 24));
        Self {
            out,
            last: Vec::new(),
            width: cols,
            sync: std::env::var("PI_NO_SYNC").is_err(),
        }
    }

    pub fn resize(&mut self, cols: u16) {
        if cols != self.width {
            // Width change invalidates word-wrapping for every line; the
            // diff renderer would otherwise compare new wrapped text against
            // old differently-wrapped text and produce visible drift. Force
            // a full repaint by dropping the previous-frame cache. The next
            // `render()` call will redraw every line unconditionally.
            self.last.clear();
            // Best-effort screen scrub so we don't leave stale glyphs from
            // the old layout. Failure here just means the next render still
            // repaints from scratch; not fatal.
            let _ = queue!(self.out, terminal::Clear(terminal::ClearType::FromCursorDown));
            let _ = self.out.flush();
        }
        self.width = cols;
    }

    pub fn render(&mut self, frame: &Frame) -> std::io::Result<()> {
        let raw: Vec<String> = frame
            .lines
            .iter()
            .map(|l| {
                let mut s = String::new();
                for span in &l.spans {
                    s.push_str(&span.text);
                }
                s
            })
            .collect();

        if self.sync {
            // DEC 2026 begin synchronized update
            self.out.write_all(b"\x1b[?2026h")?;
        }

        // Absolute anchoring. The whole alt-screen is ours: paint
        // every frame starting at (col=0, row=0). This is what
        // ratatui / blessed.js / Ink all do; relative MoveUp/MoveDown
        // off the previous render's cursor position is too fragile
        // because:
        //   * a hardware-cursor target on a mid-frame line leaves the
        //     OS cursor away from the frame's bottom;
        //   * `\n` written at the last terminal row triggers scroll
        //     instead of moving the cursor down, but the cursor
        //     position itself stays put — so MoveUp(N) afterwards
        //     undershoots by 1.
        //
        // With absolute MoveTo(0,0) the position is unambiguous and
        // every frame paints over the previous one in place.
        // Diff-rendering still applies per-line (we only emit Clear+
        // text for changed lines) so the wire cost is unchanged for
        // the common "edit one row" case.
        queue!(self.out, cursor::MoveTo(0, 0))?;

        for (i, line) in frame.lines.iter().enumerate() {
            let raw_line = raw.get(i).cloned().unwrap_or_default();
            let same = self.last.get(i).map(|s| s == &raw_line).unwrap_or(false);
            if !same {
                queue!(
                    self.out,
                    terminal::Clear(terminal::ClearType::CurrentLine),
                    cursor::MoveToColumn(0)
                )?;
                for span in &line.spans {
                    if let Some(c) = span.color {
                        queue!(self.out, SetForegroundColor(c))?;
                    }
                    self.out.write_all(span.text.as_bytes())?;
                    if span.color.is_some() {
                        queue!(self.out, ResetColor)?;
                    }
                }
            }
            // Move to start of next row instead of writing a literal
            // `\n`: at the bottom-most row a `\n` would scroll the
            // alt-screen. Absolute MoveTo never scrolls.
            if i + 1 < frame.lines.len() {
                queue!(self.out, cursor::MoveTo(0, (i + 1) as u16))?;
            }
        }

        // Clear leftover lines from previous frame: rows
        // `frame.lines.len() .. last.len()` need to be blanked out
        // because they may contain text from a taller previous frame
        // (e.g. picker overlay closed, transcript shrunk).
        let new_len = frame.lines.len();
        let old_len = self.last.len();
        if old_len > new_len {
            for row in new_len..old_len {
                queue!(
                    self.out,
                    cursor::MoveTo(0, row as u16),
                    terminal::Clear(terminal::ClearType::CurrentLine)
                )?;
            }
        }

        // Hardware cursor placement (RFD-style): if the frame named a
        // target, hop to it from the post-render position (last line of
        // the frame, end of its text) and reveal the cursor; otherwise
        // keep it hidden so picker overlays don't blink in random spots.
        if let Some((target_line, target_col)) = frame.cursor_at {
            let cur_line = frame.lines.len().saturating_sub(1) as u16;
            let target_line = target_line.min(cur_line);
            // Absolute placement — works regardless of where the
            // line-rendering loop left the cursor.
            queue!(
                self.out,
                cursor::MoveTo(target_col, target_line),
                cursor::Show
            )?;
        } else {
            queue!(self.out, cursor::Hide)?;
        }

        if self.sync {
            self.out.write_all(b"\x1b[?2026l")?;
        }
        self.out.flush()?;
        self.last = raw;
        Ok(())
    }
}
