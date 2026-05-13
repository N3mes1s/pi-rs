//! Test wrapping via textwrap: verify that wrapped lines including prefix
//! never exceed the viewport width.

use pi_coding_agent::renderer::{Block, Transcript};
use pi_tui::Theme;
use unicode_width::UnicodeWidthStr;

const LONG_PARAGRAPH: &str = "The quick brown fox jumps over the lazy dog and then it runs through the forest. In the meadow beyond the trees there are flowers of many different colors including red yellow and blue which bloom in spring and summer but fade when autumn arrives. The wind carries seeds to distant places where they take root and grow into new plants. Rivers flow from mountains to the sea passing through valleys and cities along the way. Birds migrate across continents following ancient routes guided by instinct and memory. The ocean contains vast ecosystems full of creatures that have adapted to life underwater. Coral reefs provide shelter for countless species of fish invertebrates and other marine life. Forests absorb carbon dioxide and release oxygen helping to maintain the balance of gases in the atmosphere which is essential for all living things on this planet. Mountains rise and fall over geological time shaped by the relentless forces of tectonics.";

fn dark_theme() -> Theme {
    Theme {
        name: "dark".into(),
        fg: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::White),
        bg: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::Reset),
        muted: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::DarkGrey),
        accent: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::Cyan),
        user: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::Cyan),
        assistant: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::Green),
        thinking: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::DarkGrey),
        tool: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::Yellow),
        error: pi_tui::theme::ColorSpec::Named(pi_tui::theme::NamedColor::Red),
    }
}

#[test]
fn wrap_via_textwrap_respects_viewport_with_prefix() {
    // Test that a 200-word paragraph wrapped at width 80 with "pi> " prefix
    // on first line and 4 spaces on continuation lines never exceeds 80 cols.
    let mut transcript = Transcript::default();
    transcript
        .blocks
        .push(Block::AssistantText(LONG_PARAGRAPH.to_string()));

    let theme = dark_theme();
    let viewport_width = 80u16;

    let frame = transcript.render(&theme, viewport_width);

    // Find all lines that come from the AssistantText block.
    // They should start with "pi> " or "    " (4 spaces).
    let mut found_assistant = false;
    for line in &frame.lines {
        let line_text: String = line.spans.iter().map(|s| s.text.as_str()).collect();

        // Skip empty lines and non-assistant lines (headers, footers, etc.)
        if line_text.is_empty() {
            continue;
        }

        // Check if it starts with "pi> " or "    " (the assistant prefix pattern)
        if line_text.starts_with("pi> ") || line_text.starts_with("    ") {
            found_assistant = true;
            let width = UnicodeWidthStr::width(line_text.as_str());
            assert!(
                width <= viewport_width as usize,
                "Assistant line exceeds viewport width ({}): {} cols: {:?}",
                viewport_width,
                width,
                line_text
            );
        }
    }

    assert!(
        found_assistant,
        "Should have found at least one assistant line"
    );
}

#[test]
fn wrap_via_textwrap_narrower_viewport() {
    // Same test but at width 40 to ensure aggressive wrapping works.
    let mut transcript = Transcript::default();
    transcript
        .blocks
        .push(Block::AssistantText(LONG_PARAGRAPH.to_string()));

    let theme = dark_theme();
    let viewport_width = 40u16;

    let frame = transcript.render(&theme, viewport_width);

    let mut found_assistant = false;
    for line in &frame.lines {
        let line_text: String = line.spans.iter().map(|s| s.text.as_str()).collect();

        if line_text.is_empty() {
            continue;
        }

        if line_text.starts_with("pi> ") || line_text.starts_with("    ") {
            found_assistant = true;
            let width = UnicodeWidthStr::width(line_text.as_str());
            assert!(
                width <= viewport_width as usize,
                "Line at width 40 exceeds viewport ({}): {} cols: {:?}",
                viewport_width,
                width,
                line_text
            );
        }
    }

    assert!(
        found_assistant,
        "Should have found at least one assistant line"
    );
}

#[test]
fn wrap_via_textwrap_preserves_content() {
    // Ensure wrapping doesn't lose content.
    let mut transcript = Transcript::default();
    transcript
        .blocks
        .push(Block::AssistantText(LONG_PARAGRAPH.to_string()));

    let theme = dark_theme();
    let frame = transcript.render(&theme, 80);

    let all_text: String = frame
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    // Should contain key words from the paragraph
    assert!(all_text.contains("quick"), "should contain 'quick'");
    assert!(all_text.contains("fox"), "should contain 'fox'");
    assert!(all_text.contains("Mountains"), "should contain 'Mountains'");
    assert!(all_text.contains("tectonics"), "should contain 'tectonics'");
}
