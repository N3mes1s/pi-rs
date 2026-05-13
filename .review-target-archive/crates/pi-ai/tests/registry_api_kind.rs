//! Smoke test for RFD 0019 step 1 — every model in the registry has
//! the right `ApiKind` so dispatch in
//! `crates/pi-ai/src/provider/openai.rs` picks the correct surface.
//!
//! These three rows cover the interesting cases:
//!   * `gpt-5.4`        — Responses-only (the model that triggered RFD 0019).
//!   * `gpt-4o`         — still on Chat Completions on the OpenAI side.
//!   * `claude-opus-4-7` — non-OpenAI; must default to ChatCompletions.

use pi_ai::auth::AuthStorage;
use pi_ai::{ApiKind, ModelRegistry};

fn api_kind_of(target: &str) -> ApiKind {
    let registry = ModelRegistry::new(AuthStorage::in_memory());
    let (_provider, model) = registry
        .resolve(target)
        .unwrap_or_else(|| panic!("model {target} should be in the registry"));
    model.api_kind
}

#[test]
fn gpt_5_4_uses_responses_api() {
    assert_eq!(api_kind_of("gpt-5.4"), ApiKind::Responses);
}

#[test]
fn gpt_4o_stays_on_chat_completions() {
    assert_eq!(api_kind_of("gpt-4o"), ApiKind::ChatCompletions);
}

#[test]
fn non_openai_models_default_to_chat_completions() {
    // The default for every other provider must be safe: registries
    // that don't set `api_kind` should never accidentally hit
    // /v1/responses.
    assert_eq!(api_kind_of("claude-opus-4-7"), ApiKind::ChatCompletions);
}
