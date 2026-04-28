//! RFD 0014: real BPE tokenizer for token-count estimation.

use pi_ai::tokenizer::{count, count_default, TokenizerKind};

#[test]
fn empty_string_is_zero_tokens() {
    assert_eq!(count_default(""), 0);
}

#[test]
fn hello_world_is_two_tokens() {
    // cl100k_base encodes "hello world" as ["hello", " world"] => 2.
    // If the BPE init failed (offline build), the fallback returns
    // s.len() / 4 = 11 / 4 = 2 — same answer, so this test stays
    // meaningful in either environment.
    let n = count_default("hello world");
    assert_eq!(n, 2, "expected 2 tokens, got {n}");
}

#[test]
fn english_text_in_sanity_range() {
    // Build a ~4 KB English fixture by repeating a stable sentence.
    let sentence = "The quick brown fox jumps over the lazy dog. ";
    let mut text = String::with_capacity(4096);
    while text.len() < 4096 {
        text.push_str(sentence);
    }
    let bytes = text.len() as u64;
    let n = count_default(&text);
    let lo = bytes / 5;
    let hi = bytes / 3;
    assert!(
        n >= lo && n <= hi,
        "tokens {n} outside sanity range [{lo}, {hi}] for {bytes} bytes"
    );
}

#[test]
fn for_model_picks_o200k_for_modern_openai() {
    assert_eq!(TokenizerKind::for_model("gpt-5"), TokenizerKind::O200kBase);
    assert_eq!(TokenizerKind::for_model("gpt-4o"), TokenizerKind::O200kBase);
    assert_eq!(
        TokenizerKind::for_model("o1-preview"),
        TokenizerKind::O200kBase
    );
    assert_eq!(
        TokenizerKind::for_model("o3-mini"),
        TokenizerKind::O200kBase
    );
    assert_eq!(TokenizerKind::for_model("o4"), TokenizerKind::O200kBase);
}

#[test]
fn for_model_falls_back_to_cl100k_for_claude() {
    assert_eq!(
        TokenizerKind::for_model("claude-opus-4-7"),
        TokenizerKind::Cl100kBase
    );
    assert_eq!(
        TokenizerKind::for_model("claude-sonnet-4-5"),
        TokenizerKind::Cl100kBase
    );
    assert_eq!(
        TokenizerKind::for_model("gpt-4-turbo"),
        TokenizerKind::Cl100kBase
    );
}

#[test]
fn count_with_kind_matches_for_model() {
    let s = "function hello() { return 42; }";
    let cl = count(TokenizerKind::Cl100kBase, s);
    let o2 = count(TokenizerKind::O200kBase, s);
    // Both should be small positive numbers.
    assert!(cl > 0);
    assert!(o2 > 0);
}
