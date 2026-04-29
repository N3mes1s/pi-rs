//! Tests for markdown block-level rendering: headings, lists, and blockquotes.

use crossterm::style::Color;
use pi_coding_agent::markdown::parse_and_render_markdown;
use pi_tui::Line;
use unicode_width::UnicodeWidthStr;

fn line_text(line: &Line) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

fn non_empty_lines(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(line_text)
        .filter(|text| !text.trim().is_empty())
        .collect()
}

#[test]
fn markdown_heading_renders_without_hash_prefix() {
    let lines = parse_and_render_markdown("# Heading One\n\nbody", Color::Blue, Color::DarkGrey, 40);
    let texts = non_empty_lines(&lines);

    assert_eq!(texts.first().map(String::as_str), Some("Heading One"));
    assert!(texts.iter().any(|line| line == "body"));
    assert!(!texts.join("\n").contains("# Heading One"));

    let heading_line = lines
        .iter()
        .find(|line| line_text(line) == "Heading One")
        .expect("heading line present");
    assert!(heading_line.spans.iter().all(|span| span.color == Some(Color::Blue)));
}

#[test]
fn markdown_unordered_list_renders_bullets() {
    let lines = parse_and_render_markdown("- first item\n- second item", Color::Cyan, Color::DarkGrey, 40);
    let texts = non_empty_lines(&lines);

    assert!(texts.iter().any(|line| line == "• first item"), "got {texts:?}");
    assert!(texts.iter().any(|line| line == "• second item"), "got {texts:?}");
}

#[test]
fn markdown_ordered_list_renders_indices() {
    let lines = parse_and_render_markdown("1. first\n2. second", Color::Cyan, Color::DarkGrey, 40);
    let texts = non_empty_lines(&lines);

    assert!(texts.iter().any(|line| line == "1. first"), "got {texts:?}");
    assert!(texts.iter().any(|line| line == "2. second"), "got {texts:?}");
}

#[test]
fn markdown_blockquote_renders_quote_gutter_and_wraps() {
    let lines = parse_and_render_markdown(
        "> alpha bravo charlie delta echo foxtrot golf hotel",
        Color::Cyan,
        Color::DarkGrey,
        18,
    );
    let texts = non_empty_lines(&lines);

    assert!(texts.len() > 1, "expected wrapped quote, got {texts:?}");
    for text in &texts {
        assert!(text.starts_with("│ "), "missing quote gutter: {text:?}");
        assert!(UnicodeWidthStr::width(text.as_str()) <= 18, "line too wide: {text:?}");
    }

    let joined = texts.join(" ");
    assert!(joined.contains("alpha bravo"), "lost quote content: {joined}");
    assert!(joined.contains("golf hotel"), "lost quote content: {joined}");
    assert!(!joined.contains("> alpha"), "literal blockquote marker leaked: {joined}");
}
