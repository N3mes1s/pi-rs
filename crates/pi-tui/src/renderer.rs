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

        // Move cursor to top of our output region.
        queue!(self.out, cursor::MoveToColumn(0))?;
        if !self.last.is_empty() {
            queue!(self.out, cursor::MoveUp(self.last.len() as u16))?;
        }

        for (i, line) in frame.lines.iter().enumerate() {
            let raw_line = raw.get(i).cloned().unwrap_or_default();
            let same = self.last.get(i).map(|s| s == &raw_line).unwrap_or(false);
            if !same {
                queue!(self.out, terminal::Clear(terminal::ClearType::CurrentLine), cursor::MoveToColumn(0))?;
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
            if i + 1 < frame.lines.len() {
                self.out.write_all(b"\n")?;
            }
        }

        // Clear leftover lines from previous frame.
        let leftover = self.last.len().saturating_sub(frame.lines.len());
        for _ in 0..leftover {
            self.out.write_all(b"\n")?;
            queue!(self.out, terminal::Clear(terminal::ClearType::CurrentLine))?;
        }

        if self.sync {
            self.out.write_all(b"\x1b[?2026l")?;
        }
        self.out.flush()?;
        self.last = raw;
        Ok(())
    }
}
