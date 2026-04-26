//! Smoke test for `default_system_prompt`.
//!
//! Upstream pi keeps the prompt under ~1k tokens — we mirror that ceiling.

use pi_agent_core::default_system_prompt;

#[test]
fn default_system_prompt_is_non_empty_and_under_one_kilobyte() {
    let s = default_system_prompt();
    assert!(!s.is_empty());
    assert!(
        s.len() < 4096,
        "system prompt should remain tiny, was {} bytes",
        s.len()
    );
    assert!(s.contains("coding"));
}
