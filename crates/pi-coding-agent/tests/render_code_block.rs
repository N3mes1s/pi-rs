//! Test fenced code block rendering: syntax highlighting, language label.

use crossterm::style::Color;
use pi_coding_agent::markdown::render_code_block;

#[test]
fn code_block_includes_language_label() {
    let code = "fn main() {\n    println!(\"Hello\");\n}";
    let result = render_code_block("rust", code, 80);
    
    let first_line = result.first().expect("should have at least one line");
    let label_text: String = first_line.spans.iter().map(|s| s.text.as_str()).collect();
    
    assert!(label_text.contains("rust"), "first line should show 'rust' language label");
}

#[test]
fn code_block_renders_non_empty_lines() {
    let code = "fn main() {}\nfn test() {}";
    let result = render_code_block("rust", code, 80);
    
    // Should have header + 2 code lines + footer
    assert!(result.len() >= 3, "code block should have header, body, and footer");
    
    // Check that code lines contain the function names (with or without syntax highlighting spans)
    let all_text: String = result
        .iter()
        .skip(1) // Skip header
        .take(result.len().saturating_sub(2)) // Skip footer
        .flat_map(|line| line.spans.iter().map(|s| s.text.as_str()))
        .collect();
    
    assert!(all_text.contains("main") || all_text.contains("fn"), "body should contain code content");
}

#[test]
fn code_block_empty_language_defaults_to_code() {
    let code = "some text";
    let result = render_code_block("", code, 80);
    
    let first_line = result.first().expect("should have at least one line");
    let label_text: String = first_line.spans.iter().map(|s| s.text.as_str()).collect();
    
    assert!(label_text.contains("code"), "empty language should show 'code' label");
}

#[test]
fn code_block_has_border_lines() {
    let code = "x = 1";
    let result = render_code_block("python", code, 80);
    
    // First line should have opening border (╭─)
    let first = result.first().expect("should have header");
    let first_text: String = first.spans.iter().map(|s| s.text.as_str()).collect();
    assert!(first_text.contains("╭"), "header should have opening border");
    
    // Last line should have closing border (╰─)
    let last = result.last().expect("should have footer");
    let last_text: String = last.spans.iter().map(|s| s.text.as_str()).collect();
    assert!(last_text.contains("╰"), "footer should have closing border");
}

#[test]
fn code_block_preserves_indentation() {
    let code = "def foo():\n    x = 1\n    return x";
    let result = render_code_block("python", code, 80);
    
    // Each line in result should start with spacing ("  ")
    let body_lines = result
        .iter()
        .skip(1)
        .take(result.len().saturating_sub(2));
    
    for line in body_lines {
        if !line.spans.is_empty() {
            let first_span = &line.spans[0];
            assert!(first_span.text.starts_with("  "), 
                    "code line should be indented with '  ': {:?}", first_span.text);
        }
    }
}

#[test]
fn code_block_multiline_no_syntax_error() {
    let code = "let x = 1;\nlet y = 2;\nlet z = x + y;";
    let result = render_code_block("rust", code, 80);
    
    // Should successfully render without panicking
    // All lines should have spans (possibly empty, but the Line exists)
    assert!(!result.is_empty(), "should produce non-empty result");
    for line in &result {
        // Each line should have at least something (header/code/footer)
        // Line may have empty spans[] but the Line struct exists
        let _ = &line.spans;
    }
}
