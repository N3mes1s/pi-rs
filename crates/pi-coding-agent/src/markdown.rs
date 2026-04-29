//! Markdown rendering: parse markdown into styled spans with proper text wrapping.
//!
//! Uses pulldown-cmark v0.9 for markdown parsing and syntect v5 for
//! syntax highlighting in fenced code blocks.

use crossterm::style::Color;
use pi_tui::{Line, Span};
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Render markdown text into pi-tui `Line`s with proper wrapping and styling.
///
/// - `**bold**` / `__bold__` → accent colour
/// - `_italic_` / `*italic*`  → muted colour
/// - `` `code` ``              → cyan colour, no backticks
/// - Fenced code blocks        → syntax-highlighted with language label
///
/// All inline asterisks and backticks are consumed by the parser and never
/// appear literally in the output.
pub fn parse_and_render_markdown(
    text: &str,
    accent: Color,
    muted: Color,
    viewport_cols: u16,
) -> Vec<Line> {
    let wrap_width = (viewport_cols as usize).saturating_sub(2).max(10);
    let parser = Parser::new(text);
    let mut result: Vec<Line> = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();

    let mut bold_depth = 0u32;
    let mut italic_depth = 0u32;

    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_body = String::new();

    let flush_wrapped_paragraph =
        |result: &mut Vec<Line>, current: &mut Vec<Span>| {
            if current.is_empty() {
                return;
            }
            result.extend(wrap_spans(std::mem::take(current), wrap_width));
        };

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_wrapped_paragraph(&mut result, &mut current_spans);
                in_code_block = true;
                code_lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                code_body.clear();
            }
            Event::Start(Tag::Paragraph) => {}

            Event::End(Tag::CodeBlock(_)) => {
                in_code_block = false;
                result.push(Line::default());
                result.extend(render_code_block(&code_lang, &code_body, viewport_cols));
                result.push(Line::default());
            }
            Event::End(Tag::Paragraph) => {
                flush_wrapped_paragraph(&mut result, &mut current_spans);
                result.push(Line::default());
            }

            Event::Start(Tag::Strong) => bold_depth += 1,
            Event::Start(Tag::Emphasis) => italic_depth += 1,
            Event::End(Tag::Strong) => bold_depth = bold_depth.saturating_sub(1),
            Event::End(Tag::Emphasis) => italic_depth = italic_depth.saturating_sub(1),

            Event::Code(text) => {
                current_spans.push(Span::coloured(text.into_string(), Color::Cyan));
            }
            Event::Text(text) => {
                if in_code_block {
                    code_body.push_str(&text);
                    continue;
                }
                let color = if bold_depth > 0 {
                    Some(accent)
                } else if italic_depth > 0 {
                    Some(muted)
                } else {
                    None
                };
                let span = match color {
                    Some(c) => Span::coloured(text.into_string(), c),
                    None => Span::plain(text.into_string()),
                };
                current_spans.push(span);
            }

            Event::SoftBreak | Event::HardBreak => {
                flush_wrapped_paragraph(&mut result, &mut current_spans);
            }

            _ => {}
        }
    }

    flush_wrapped_paragraph(&mut result, &mut current_spans);

    while result.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
        result.pop();
    }

    if result.is_empty() {
        result.push(Line::default());
    }

    result
}

fn wrap_spans(spans: Vec<Span>, width: usize) -> Vec<Line> {
    if spans.is_empty() {
        return Vec::new();
    }
    if width == 0 {
        return vec![Line { spans }];
    }

    let mut out: Vec<Line> = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut current_width = 0usize;

    for span in spans {
        let mut remaining = span.text.as_str();
        while !remaining.is_empty() {
            if current.is_empty() {
                remaining = remaining.trim_start_matches(char::is_whitespace);
                if remaining.is_empty() {
                    break;
                }
            }

            if current_width >= width {
                out.push(Line {
                    spans: std::mem::take(&mut current),
                });
                current_width = 0;
            }

            let available = width.saturating_sub(current_width);
            if available == 0 {
                out.push(Line {
                    spans: std::mem::take(&mut current),
                });
                current_width = 0;
                continue;
            }

            let (take, rest) = split_for_wrap(remaining, available);
            if take.is_empty() {
                if !current.is_empty() {
                    out.push(Line {
                        spans: std::mem::take(&mut current),
                    });
                    current_width = 0;
                    continue;
                }
                break;
            }

            let mut piece = span.clone();
            piece.text = take.to_string();
            current_width += UnicodeWidthStr::width(piece.text.as_str());
            current.push(piece);
            remaining = rest;

            if !remaining.is_empty() {
                out.push(Line {
                    spans: std::mem::take(&mut current),
                });
                current_width = 0;
            }
        }
    }

    if !current.is_empty() {
        out.push(Line { spans: current });
    }

    out
}

fn split_for_wrap(text: &str, available: usize) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }
    if available == 0 {
        return ("", text);
    }

    let mut width = 0usize;
    let mut last_whitespace_end = None;
    for (idx, ch) in text.char_indices() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > available {
            if let Some(split) = last_whitespace_end {
                return (&text[..split], &text[split..]);
            }
            if idx == 0 {
                let next = idx + ch.len_utf8();
                return (&text[..next], &text[next..]);
            }
            return (&text[..idx], &text[idx..]);
        }
        width += ch_width;
        if ch.is_whitespace() {
            last_whitespace_end = Some(idx);
        }
    }

    (text, "")
}

/// Render a fenced code block with syntect syntax highlighting.
///
/// Returns a `Vec<Line>` with:
/// - A header line showing the language label
/// - Syntax-highlighted body lines
/// - A footer/border line
pub fn render_code_block(lang: &str, code: &str, _viewport_cols: u16) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();

    let lang_label = if lang.is_empty() { "code" } else { lang };
    lines.push(Line {
        spans: vec![Span::coloured(
            format!("  ╭─ {} ", lang_label),
            Color::DarkGrey,
        )],
    });

    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = ts
        .themes
        .get("base16-ocean.dark")
        .or_else(|| ts.themes.values().next())
        .expect("syntect has at least one bundled theme");

    let syntax = if !lang.is_empty() {
        ss.find_syntax_by_token(lang)
            .or_else(|| ss.find_syntax_by_extension(lang))
    } else {
        None
    }
    .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut hl = HighlightLines::new(syntax, theme);

    for line_text in LinesWithEndings::from(code) {
        let trimmed = line_text.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            lines.push(Line {
                spans: vec![Span::plain("  ".to_string())],
            });
            continue;
        }

        match hl.highlight_line(trimmed, &ss) {
            Ok(ranges) => {
                let mut spans: Vec<Span> = vec![Span::plain("  ".to_string())];
                for (style, text) in ranges {
                    let color = Color::Rgb {
                        r: style.foreground.r,
                        g: style.foreground.g,
                        b: style.foreground.b,
                    };
                    spans.push(Span::coloured(text.to_string(), color));
                }
                lines.push(Line { spans });
            }
            Err(_) => {
                lines.push(Line {
                    spans: vec![Span::plain(format!("  {}", trimmed))],
                });
            }
        }
    }

    lines.push(Line {
        spans: vec![Span::coloured("  ╰─".to_string(), Color::DarkGrey)],
    });

    lines
}
