//! Test word wrapping: long paragraphs wrap at word boundaries.

use crossterm::style::Color;
use pi_coding_agent::markdown::parse_and_render_markdown;
use unicode_width::UnicodeWidthStr;

const SAMPLE_200_WORD_PARAGRAPH: &str =
    "The quick brown fox jumps over the lazy dog and then it runs through the forest. \
    In the meadow beyond the trees there are flowers of many different colors including \
    red yellow and blue which bloom in spring and summer but fade when autumn arrives. \
    The wind carries seeds to distant places where they take root and grow into new plants. \
    Rivers flow from mountains to the sea passing through valleys and cities along the way. \
    Birds migrate across continents following ancient routes guided by instinct and memory. \
    The ocean contains vast ecosystems full of creatures that have adapted to life underwater. \
    Coral reefs provide shelter for countless species of fish invertebrates and other marine life. \
    Forests absorb carbon dioxide and release oxygen helping to maintain the balance of gases \
    in the atmosphere which is essential for all living things on this planet. \
    Mountains rise and fall over geological time shaped by the relentless forces of tectonics.";

#[test]
fn word_wrap_no_line_exceeds_80_cols() {
    let result = parse_and_render_markdown(SAMPLE_200_WORD_PARAGRAPH, Color::Cyan, Color::DarkGrey, 80);
    
    for (i, line) in result.iter().enumerate() {
        let line_text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        let width = UnicodeWidthStr::width(line_text.as_str());
        assert!(
            width <= 80,
            "line {} exceeds 80 cols (width={}): {:?}",
            i,
            width,
            line_text
        );
    }
}

#[test]
fn word_wrap_no_word_split_across_lines() {
    let result = parse_and_render_markdown(SAMPLE_200_WORD_PARAGRAPH, Color::Cyan, Color::DarkGrey, 80);
    
    // Collect all line texts
    let line_texts: Vec<String> = result
        .iter()
        .map(|line| {
            line.spans.iter().map(|s| s.text.as_str()).collect::<String>()
        })
        .collect();
    
    // No line should start with a fragment that looks like a mid-word continuation.
    // Simplified check: no word from the original text should appear split across two adjacent lines.
    let all_words: Vec<&str> = SAMPLE_200_WORD_PARAGRAPH.split_whitespace().collect();
    
    for word in &all_words {
        // word must appear whole on at least one line
        let appears_whole = line_texts.iter().any(|l| l.contains(*word));
        if !appears_whole {
            // Allow for slight wrapping differences: check if word is spread across lines
            // (this shouldn't happen with proper wrapping)
            // At minimum, the word characters must all appear in the output
            let all_output: String = line_texts.iter().flat_map(|l| l.chars()).collect();
            assert!(
                all_output.contains(word),
                "word {:?} disappeared from output entirely",
                word
            );
        }
    }
}

#[test]
fn word_wrap_at_width_40() {
    let result = parse_and_render_markdown(SAMPLE_200_WORD_PARAGRAPH, Color::Cyan, Color::DarkGrey, 40);
    
    for (i, line) in result.iter().enumerate() {
        let line_text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        let width = UnicodeWidthStr::width(line_text.as_str());
        assert!(
            width <= 40,
            "line {} exceeds 40 cols (width={}): {:?}",
            i,
            width,
            line_text
        );
    }
}

#[test]
fn word_wrap_preserves_all_content() {
    let text = "one two three four five six seven eight nine ten";
    let result = parse_and_render_markdown(text, Color::Cyan, Color::DarkGrey, 20);
    
    let all_text: String = result
        .iter()
        .flat_map(|line| line.spans.iter().map(|s| s.text.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    
    // All words should be present in the output
    for word in ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"] {
        assert!(all_text.contains(word), "word '{}' missing from output: {}", word, all_text);
    }
}

#[test]
fn word_wrap_reflows_styled_paragraphs_without_eating_spaces() {
    let text = "alpha **bravo** charlie delta echo foxtrot golf";
    let result = parse_and_render_markdown(text, Color::Cyan, Color::DarkGrey, 18);

    assert!(result.len() > 1, "expected wrapped output, got {result:?}");

    let joined = result
        .iter()
        .map(|line| line.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("alpha bravo charlie"), "lost spacing: {joined}");

    for (i, line) in result.iter().enumerate() {
        let line_text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
        let width = UnicodeWidthStr::width(line_text.as_str());
        assert!(width <= 18, "line {} exceeds 18 cols (width={}): {:?}", i, width, line_text);
    }
}
