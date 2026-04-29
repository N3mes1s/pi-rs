//! Integration test: Transcript renders AssistantText with markdown parsing.

use pi_coding_agent::renderer::{Block, Transcript};
use pi_tui::Theme;

#[test]
fn transcript_assistant_text_renders_markdown_bold_inline() {
    let mut transcript = Transcript::default();
    transcript.blocks.push(Block::AssistantText(
        "The answer is **bold** and here.".to_string(),
    ));

    let theme = Theme {
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
    };

    let frame = transcript.render(&theme, 80);

    let all_text: String = frame
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    // Should contain the text without literal asterisks
    assert!(
        all_text.contains("bold"),
        "should contain 'bold': {}",
        all_text
    );
    assert!(
        all_text.contains("answer"),
        "should contain 'answer': {}",
        all_text
    );
    assert!(
        !all_text.contains("**"),
        "should not contain literal asterisks: {}",
        all_text
    );

    // Should have "pi>" prefix
    assert!(
        all_text.contains("pi>"),
        "should have 'pi>' prefix: {}",
        all_text
    );
}

#[test]
fn transcript_assistant_text_renders_markdown_inline_code() {
    let mut transcript = Transcript::default();
    transcript.blocks.push(Block::AssistantText(
        "Use the `println!` macro in Rust.".to_string(),
    ));

    let theme = Theme {
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
    };

    let frame = transcript.render(&theme, 80);

    let all_text: String = frame
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    // Should contain macro name without backticks
    assert!(
        all_text.contains("println!"),
        "should contain 'println!': {}",
        all_text
    );
    assert!(
        !all_text.contains("`"),
        "should not contain backticks: {}",
        all_text
    );
}

#[test]
fn transcript_assistant_text_with_fenced_code_block() {
    let code = "fn main() {\n    println!(\"hello\");\n}";
    let mut transcript = Transcript::default();
    transcript.blocks.push(Block::AssistantText(format!(
        "Here is a function:\n\n```rust\n{}\n```\n\nDone.",
        code
    )));

    let theme = Theme {
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
    };

    let frame = transcript.render(&theme, 80);

    let all_text: String = frame
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
        .collect();

    // Should contain function name and "rust" label
    assert!(
        all_text.contains("main"),
        "should contain 'main': {}",
        all_text
    );
    assert!(
        all_text.contains("rust"),
        "should contain 'rust' language label: {}",
        all_text
    );

    // Should contain intro and outro text
    assert!(
        all_text.contains("Here is a function"),
        "intro text: {}",
        all_text
    );
    assert!(all_text.contains("Done"), "outro text: {}", all_text);
}
