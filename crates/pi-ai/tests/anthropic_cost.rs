//! Unit tests for `compute_cost` in the Anthropic provider (RFD 0008).

use pi_ai::provider::anthropic::{compute_cost, UsageAcc};
use pi_ai::registry::ModelInfo;

fn opus_4_7() -> ModelInfo {
    ModelInfo {
        provider: "anthropic".into(),
        id: "claude-opus-4-7".into(),
        alias: Some("opus".into()),
        context_window: 200_000,
        max_output_tokens: 32_000,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 15.0,
        output_cost_per_mtok: 75.0,
    }
}

#[test]
fn one_million_in_one_million_out_equals_input_plus_output_rates() {
    let model = opus_4_7();
    let u = UsageAcc {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        ..Default::default()
    };
    let cost = compute_cost(&model, &u);
    // 15.0 + 75.0 = 90.0
    assert!((cost - 90.0).abs() < 1e-9, "got {cost}");
}

#[test]
fn cache_read_tokens_fold_into_input_rate() {
    let model = opus_4_7();
    let u = UsageAcc {
        input_tokens: 0,
        cache_read_tok: 1_000_000,
        ..Default::default()
    };
    let cost = compute_cost(&model, &u);
    // Cache reads currently billed at the flat input rate (RFD 0008
    // defers differential pricing to RFD 0009).
    assert!((cost - 15.0).abs() < 1e-9, "got {cost}");
}

#[test]
fn cache_write_and_reasoning_also_count() {
    let model = opus_4_7();
    let u = UsageAcc {
        cache_write_tok: 1_000_000,
        reasoning_tok: 1_000_000,
        ..Default::default()
    };
    let cost = compute_cost(&model, &u);
    // 1M cache_write @ input rate (15) + 1M reasoning @ output rate (75) = 90.
    assert!((cost - 90.0).abs() < 1e-9, "got {cost}");
}
