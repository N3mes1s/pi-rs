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

    // Style stacks
    let mut bold_depth = 0u32;
    let mut italic_depth = 0u32;

    // Code block state
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_body = String::new();

    let flush_line = |result: &mut Vec<Line>, current: &mut Vec<Span>| {
        if !current.is_empty() {
            result.push(Line {
                spans: std::mem::take(current),
            });
        }
    };

    for event in parser {
        match event {
            // ── Block start tags ───────────────────────────────────────────
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_line(&mut result, &mut current_spans);
                in_code_block = true;
                code_lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                code_body.clear();
            }
            Event::Start(Tag::Paragraph) => {}

            // ── Block end tags ────────────────────────────────────────────
            Event::End(Tag::CodeBlock(_)) => {
                in_code_block = false;
                result.push(Line::default());
                result.extend(render_code_block(&code_lang, &code_body, viewport_cols));
                result.push(Line::default());
            }
            Event::End(Tag::Paragraph) => {
                flush_line(&mut result, &mut current_spans);
                result.push(Line::default());
            }

            // ── Inline style tags (start) ──────────────────────────────────
            Event::Start(Tag::Strong) => bold_depth += 1,
            Event::Start(Tag::Emphasis) => italic_depth += 1,

            // ── Inline style tags (end) ────────────────────────────────────
            Event::End(Tag::Strong) => bold_depth = bold_depth.saturating_sub(1),
            Event::End(Tag::Emphasis) => italic_depth = italic_depth.saturating_sub(1),

            // ── Inline code (backtick) ─────────────────────────────────────
            // In pulldown-cmark 0.9, inline code spans produce Event::Code(_)
            // directly rather than Start/End tags.

            // ── Text content ───────────────────────────────────────────────
            Event::Code(text) => {
                // Inline `code` — never produces literal backticks
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
                // DO NOT call wrap_line on this fragment. The wrap
                // helper trims leading/trailing whitespace at line
                // boundaries, which is right for FULL lines but wrong
                // for sub-fragments that pulldown-cmark splits at
                // emphasis boundaries. Concretely: input `"are
                // **wrap** ("` parses as Text("are "), Start(Strong),
                // Text("wrap"), End(Strong), Text(" ("). Wrapping
                // each Text individually returns ["are"] / ["wrap"]
                // / ["("] — the space-only boundaries are dropped,
                // and the rendered line reads "arewrap(" instead of
                // "are wrap (". Push the text verbatim into a Span;
                // paragraph-level word-wrap is handled by the wrap
                // pass over the assembled line if/when long lines
                // need breaking. Long fully-styled lines may overflow
                // the viewport for now — tracked as a follow-up; the
                // visible space-eating bug is the user-impacting one.
                let span = match color {
                    Some(c) => Span::coloured(text.into_string(), c),
                    None => Span::plain(text.into_string()),
                };
                current_spans.push(span);
            }

            // ── Soft/hard breaks ───────────────────────────────────────────
            Event::SoftBreak | Event::HardBreak => {
                flush_line(&mut result, &mut current_spans);
            }

            _ => {}
        }
    }

    flush_line(&mut result, &mut current_spans);

    // Remove trailing empty lines
    while result.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
        result.pop();
    }

    if result.is_empty() {
        result.push(Line::default());
    }

    result
}

/// Render a fenced code block with syntect syntax highlighting.
///
/// Returns a `Vec<Line>` with:
/// - A header line showing the language label
/// - Syntax-highlighted body lines
/// - A footer/border line
pub fn render_code_block(lang: &str, code: &str, _viewport_cols: u16) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();

    // Language label header
    let lang_label = if lang.is_empty() { "code" } else { lang };
    lines.push(Line {
        spans: vec![Span::coloured(
            format!("  ╭─ {} ", lang_label),
            Color::DarkGrey,
        )],
    });

    // Load syntect defaults (bundled via the `parsing` feature)
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

    // Closing border line
    lines.push(Line {
        spans: vec![Span::coloured("  ╰─".to_string(), Color::DarkGrey)],
    });

    lines
}
