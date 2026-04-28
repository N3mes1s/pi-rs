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
        cache_read_cost_per_mtok: None,
        cache_write_cost_per_mtok: None,
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

// --- RFD 0010: differential cache pricing -----------------------------

fn opus_4_7_with_cache(read: Option<f64>, write: Option<f64>) -> ModelInfo {
    ModelInfo {
        provider: "anthropic".into(),
        id: "claude-opus-4-7".into(),
        alias: Some("opus".into()),
        context_window: 200_000,
        max_output_tokens: 32_000,
        supports_thinking: true,
        supports_tools: true,
        supports_vision: true,
        input_cost_per_mtok: 5.0,
        output_cost_per_mtok: 25.0,
        cache_read_cost_per_mtok: read,
        cache_write_cost_per_mtok: write,
    }
}

#[test]
fn cache_read_uses_dedicated_rate_when_set() {
    let model = opus_4_7_with_cache(Some(0.50), Some(6.25));
    let u = UsageAcc {
        cache_read_tok: 1_000_000,
        ..Default::default()
    };
    let cost = compute_cost(&model, &u);
    // 1M cache_read @ 0.50 = $0.50
    assert!((cost - 0.50).abs() < 1e-9, "got {cost}");
}

#[test]
fn cache_write_uses_dedicated_rate_when_set() {
    let model = opus_4_7_with_cache(Some(0.50), Some(6.25));
    let u = UsageAcc {
        cache_write_tok: 1_000_000,
        ..Default::default()
    };
    let cost = compute_cost(&model, &u);
    // 1M cache_write @ 6.25 = $6.25
    assert!((cost - 6.25).abs() < 1e-9, "got {cost}");
}

#[test]
fn cache_fields_none_falls_back_to_input_rate() {
    // Regression guard for RFD-0008 fallback path: if a row doesn't
    // declare cache rates, cache_read/cache_write tokens still bill at
    // input rate (byte-identical to pre-RFD-0010 behaviour).
    let model = opus_4_7_with_cache(None, None);
    let u = UsageAcc {
        cache_read_tok: 1_000_000,
        cache_write_tok: 1_000_000,
        ..Default::default()
    };
    let cost = compute_cost(&model, &u);
    // 1M @ 5.0 + 1M @ 5.0 = 10.0
    assert!((cost - 10.0).abs() < 1e-9, "got {cost}");
}

// --- RFD 0015: UsageAcc::into_usage round-trip ------------------------

#[test]
fn into_usage_matches_compute_cost_directly() {
    let model = opus_4_7();
    let acc = UsageAcc {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_read_tok: 1_000_000,
        cache_write_tok: 0,
        reasoning_tok: 0,
    };
    let direct = compute_cost(&model, &acc);
    let usage = acc.into_usage(&model);
    assert!((usage.cost_usd - direct).abs() < 1e-9, "cost mismatch");
    assert_eq!(usage.input_tokens, 1_000_000);
    assert_eq!(usage.output_tokens, 1_000_000);
    assert_eq!(usage.cache_read_tokens, 1_000_000);
    assert_eq!(usage.cache_write_tokens, 0);
    assert_eq!(usage.reasoning_tokens, 0);
}
