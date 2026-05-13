//! Test markdown inline rendering: bold, italic, inline code without literal markers.

use crossterm::style::Color;
use pi_coding_agent::markdown::parse_and_render_markdown;

#[test]
fn markdown_bold_renders_without_asterisks() {
    let text = "This is **bold text** in a sentence.";
    let result = parse_and_render_markdown(text, Color::Cyan, Color::DarkGrey, 80);

    // Collect all text from spans
    let all_text: String = result
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    // Should contain "bold text" but NOT "**bold text**"
    assert!(
        all_text.contains("bold text"),
        "result should contain 'bold text': {}",
        all_text
    );
    assert!(
        !all_text.contains("**"),
        "result should not contain asterisks: {}",
        all_text
    );
}

#[test]
fn markdown_italic_renders_without_asterisks() {
    let text = "This is _italic text_ here.";
    let result = parse_and_render_markdown(text, Color::Cyan, Color::DarkGrey, 80);

    let all_text: String = result
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    assert!(
        all_text.contains("italic text"),
        "result should contain 'italic text': {}",
        all_text
    );
    assert!(
        !all_text.contains("_"),
        "result should not contain underscores: {}",
        all_text
    );
}

#[test]
fn markdown_inline_code_renders_without_backticks() {
    let text = "Use the `println!` macro to debug.";
    let result = parse_and_render_markdown(text, Color::Cyan, Color::DarkGrey, 80);

    let all_text: String = result
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    assert!(
        all_text.contains("println!"),
        "result should contain 'println!': {}",
        all_text
    );
    assert!(
        !all_text.contains("`"),
        "result should not contain backticks: {}",
        all_text
    );
}

#[test]
fn markdown_combined_styling_no_literal_markers() {
    let text = "The **bold** and _italic_ and `code` words exist.";
    let result = parse_and_render_markdown(text, Color::Cyan, Color::DarkGrey, 80);

    let all_text: String = result
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    assert!(all_text.contains("bold"), "should have 'bold'");
    assert!(all_text.contains("italic"), "should have 'italic'");
    assert!(all_text.contains("code"), "should have 'code'");
    assert!(
        !all_text.contains("**") && !all_text.contains("_") && !all_text.contains("`"),
        "should have no literal markers: {}",
        all_text
    );
}

#[test]
fn markdown_bold_spans_have_accent_color() {
    let text = "The **bold text** is here.";
    let accent = Color::Blue;
    let result = parse_and_render_markdown(text, accent, Color::DarkGrey, 80);

    let bold_spans: Vec<_> = result
        .iter()
        .flat_map(|line| &line.spans)
        .filter(|span| span.text.contains("bold") || span.text.contains("text"))
        .collect();

    assert!(
        !bold_spans.is_empty(),
        "should find spans containing 'bold' or 'text'"
    );
    assert!(
        bold_spans.iter().any(|s| s.color == Some(accent)),
        "bold text should have accent color"
    );
}
