//! Per-model cost helper for `pi-sdk`.
//!
//! Per RFD 0027 §1 + Commit E: every embedder writes the same
//! per-model price table. Ship one.
//! [`CostRegistry::with_bundled_defaults`] returns the seeded
//! best-effort table; [`CostRegistry::default`] (and
//! [`CostRegistry::empty`]) is empty. Embedders override individual
//! entries via [`CostRegistry::override_for`].
//!
//! The underlying price math (`pi_ai::compute_cost`) already exists
//! and is shared across providers. This module is the embedder-facing
//! surface: a price-table abstraction + a single `estimate_cost_usd`
//! call site that doesn't require constructing a full `ModelInfo`.
//!
//! Prices are in USD-per-million-input-tokens. Refreshed each MINOR.
//! Embedders running with contract pricing should always
//! `override_for` rather than rely on the bundled defaults.
//!
//! ## Stability
//!
//! `CostRegistry` is `#[non_exhaustive]`-style (private fields, only
//! `default()` + `override_for` for construction). `Pricing` is a POD
//! marked `#[non_exhaustive]` so cache-tier rates can be added in a
//! MINOR.

use pi_ai::{compute_cost, ApiKind, ModelInfo, Usage, UsageAcc};
use std::collections::HashMap;

/// Per-million-token rates for one model. Embedders supply this when
/// overriding the bundled price table; the SDK feeds it into
/// `pi_ai::compute_cost` under the hood.
///
/// `cache_read_per_mtok` / `cache_write_per_mtok = None` means the
/// model bills cached input at the same rate as fresh input — the
/// RFD-0008 fallback path.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
}

impl Pricing {
    /// Construct a `Pricing` from input/output rates only. Cache rates
    /// inherit from input. Useful for back-of-envelope estimates.
    pub fn flat(input_per_mtok: f64, output_per_mtok: f64) -> Self {
        Self {
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
        }
    }

    /// Construct a `Pricing` with explicit cache-read/write rates.
    pub fn with_cache(
        input_per_mtok: f64,
        output_per_mtok: f64,
        cache_read_per_mtok: f64,
        cache_write_per_mtok: f64,
    ) -> Self {
        Self {
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: Some(cache_read_per_mtok),
            cache_write_per_mtok: Some(cache_write_per_mtok),
        }
    }

    /// Compute the USD cost of a `Usage` against this pricing
    /// directly, without needing a `CostRegistry` lookup.
    ///
    /// Useful when the embedder already has a `Pricing` in hand
    /// (e.g. from `CostRegistry::get(model)`) and wants to compute
    /// the cost without re-running the registry indirection. Output
    /// matches `estimate_cost_usd(usage, model_id, &registry)` for
    /// the same `Pricing` value.
    ///
    /// Cache rates fall back to `input_per_mtok` when None — same
    /// behavior as `pi_ai::compute_cost`.
    pub fn cost_for(&self, usage: &Usage) -> f64 {
        let cache_read_rate = self.cache_read_per_mtok.unwrap_or(self.input_per_mtok);
        let cache_write_rate = self.cache_write_per_mtok.unwrap_or(self.input_per_mtok);
        let in_dollars = (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_mtok
            + (usage.cache_read_tokens as f64 / 1_000_000.0) * cache_read_rate
            + (usage.cache_write_tokens as f64 / 1_000_000.0) * cache_write_rate;
        let out_tok = usage.output_tokens + usage.reasoning_tokens;
        let out_dollars = (out_tok as f64 / 1_000_000.0) * self.output_per_mtok;
        in_dollars + out_dollars
    }
}

/// In-memory map of model_id → `Pricing`.
///
/// [`with_bundled_defaults`](Self::with_bundled_defaults) seeds a
/// best-effort table refreshed each MINOR. [`default`](Self::default)
/// (and [`empty`](Self::empty)) returns an empty registry. Use
/// [`override_for`](Self::override_for) to pin contract pricing.
#[derive(Clone, Debug, Default)]
pub struct CostRegistry {
    prices: HashMap<String, Pricing>,
}

impl CostRegistry {
    /// Empty registry. Lookup falls through to `Pricing::flat(0.0, 0.0)`,
    /// which makes every cost calculation return `0.0`. Useful for tests
    /// where price math should not fail but should also not produce noise.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Bundled best-effort price table. Refreshed each `pi-sdk` MINOR.
    /// Numbers below were correct as of the date of this commit
    /// (2026-05-03); they MAY be stale by the time you read this.
    /// Embedders running in production should always
    /// `override_for(model_id, ...)` against their actual contract.
    pub fn with_bundled_defaults() -> Self {
        let mut r = Self::default();
        // Anthropic — list prices, USD per MTok.
        r.prices.insert("claude-opus-4-7".into(), Pricing::with_cache(15.00, 75.00, 1.50, 18.75));
        r.prices.insert("claude-opus-4-6".into(), Pricing::with_cache(15.00, 75.00, 1.50, 18.75));
        r.prices.insert("claude-sonnet-4-6".into(), Pricing::with_cache(3.00, 15.00, 0.30, 3.75));
        r.prices.insert("claude-haiku-4-5-20251001".into(), Pricing::with_cache(1.00, 5.00, 0.10, 1.25));
        // OpenAI — list prices.
        r.prices.insert("gpt-5".into(), Pricing::flat(2.50, 10.00));
        r.prices.insert("gpt-5.4".into(), Pricing::flat(3.00, 12.00));
        r.prices.insert("gpt-4.1".into(), Pricing::flat(2.00, 8.00));
        r.prices.insert("o3".into(), Pricing::flat(15.00, 60.00));
        // Google.
        r.prices.insert("gemini-2.5-pro".into(), Pricing::flat(1.25, 10.00));
        r
    }

    /// Override (or insert) the pricing for a single model_id.
    /// Returns `&mut self` for chaining.
    pub fn override_for(&mut self, model_id: impl Into<String>, pricing: Pricing) -> &mut Self {
        self.prices.insert(model_id.into(), pricing);
        self
    }

    /// Look up pricing for a model_id. Returns `None` if unknown;
    /// callers typically fall back to `Pricing::flat(0.0, 0.0)` or
    /// log a warning.
    pub fn get(&self, model_id: &str) -> Option<&Pricing> {
        self.prices.get(model_id)
    }

    /// Number of model_ids with explicit pricing entries.
    pub fn len(&self) -> usize {
        self.prices.len()
    }

    /// True iff no pricing entries.
    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }
}

/// Estimate the USD cost of one streamed turn, given its accumulated
/// `Usage` event and the model_id that produced it.
///
/// If the model_id is unknown to `registry`, returns whatever
/// `usage.cost_usd` already carries (which the provider may have
/// already populated via `compute_cost`); if that is also zero,
/// returns 0.0.
///
/// Embedders typically call this from their `AgentEvent::Usage`
/// handler:
///
/// ```no_run
/// use pi_sdk::cost::{estimate_cost_usd, CostRegistry};
/// # use pi_sdk::Usage;
/// # fn run(usage: Usage) {
/// let registry = CostRegistry::with_bundled_defaults();
/// let model_id = "claude-haiku-4-5-20251001";
/// let dollars = estimate_cost_usd(&usage, model_id, &registry);
/// eprintln!("turn cost: ${dollars:.4}");
/// # }
/// ```
pub fn estimate_cost_usd(usage: &Usage, model_id: &str, registry: &CostRegistry) -> f64 {
    if let Some(pricing) = registry.get(model_id) {
        // Reconstruct a synthetic ModelInfo so we can re-use the
        // already-tested `pi_ai::compute_cost`. ModelInfo has no
        // `Default` impl, so we fill every field; only the cost
        // fields actually matter for the math.
        let model = ModelInfo {
            provider: String::new(),
            id: model_id.into(),
            alias: None,
            context_window: 0,
            max_output_tokens: 0,
            tier: 1,
            supports_thinking: false,
            supports_tools: false,
            supports_vision: false,
            input_cost_per_mtok: pricing.input_per_mtok,
            output_cost_per_mtok: pricing.output_per_mtok,
            cache_read_cost_per_mtok: pricing.cache_read_per_mtok,
            cache_write_cost_per_mtok: pricing.cache_write_per_mtok,
            api_kind: ApiKind::default(),
        };
        let acc = UsageAcc {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tok: usage.cache_read_tokens,
            cache_write_tok: usage.cache_write_tokens,
            reasoning_tok: usage.reasoning_tokens,
        };
        compute_cost(&model, &acc)
    } else if usage.cost_usd > 0.0 {
        // Fall back to whatever the provider populated.
        usage.cost_usd
    } else {
        0.0
    }
}

/// Sum the per-turn costs across multiple `Usage` events for one model.
/// Embedders aggregating per-session totals call this on the iterator
/// of `Usage` events they observed during the session.
pub fn sum_session_cost_usd<'a, I>(usages: I, model_id: &str, registry: &CostRegistry) -> f64
where
    I: IntoIterator<Item = &'a Usage>,
{
    usages
        .into_iter()
        .map(|u| estimate_cost_usd(u, model_id, registry))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cost_usd: 0.0,
        }
    }

    #[test]
    fn empty_registry_returns_zero_for_unknown_model() {
        let r = CostRegistry::empty();
        let dollars = estimate_cost_usd(&usage(1_000, 2_000), "claude-opus-4-7", &r);
        assert_eq!(dollars, 0.0);
    }

    #[test]
    fn bundled_defaults_price_haiku() {
        let r = CostRegistry::with_bundled_defaults();
        // 1M input @ $1.00/MTok, 1M output @ $5.00/MTok = $6.00.
        let dollars =
            estimate_cost_usd(&usage(1_000_000, 1_000_000), "claude-haiku-4-5-20251001", &r);
        assert!((dollars - 6.0).abs() < 0.0001, "got {dollars}");
    }

    #[test]
    fn override_for_replaces_bundled_price() {
        let mut r = CostRegistry::with_bundled_defaults();
        r.override_for("claude-haiku-4-5-20251001", Pricing::flat(0.50, 2.50));
        // 1M input @ $0.50/MTok, 1M output @ $2.50/MTok = $3.00.
        let dollars =
            estimate_cost_usd(&usage(1_000_000, 1_000_000), "claude-haiku-4-5-20251001", &r);
        assert!((dollars - 3.0).abs() < 0.0001, "got {dollars}");
    }

    #[test]
    fn fall_back_to_provider_populated_cost_usd_when_model_unknown() {
        let r = CostRegistry::empty();
        let mut u = usage(1_000, 2_000);
        u.cost_usd = 0.123;
        let dollars = estimate_cost_usd(&u, "unknown-model", &r);
        assert_eq!(dollars, 0.123);
    }

    #[test]
    fn sum_session_cost_aggregates_across_turns() {
        let r = CostRegistry::with_bundled_defaults();
        let usages = vec![
            usage(500_000, 500_000),  // $0.50 input + $2.50 output = $3.00
            usage(500_000, 500_000),  // $3.00
        ];
        let total = sum_session_cost_usd(usages.iter(), "claude-haiku-4-5-20251001", &r);
        assert!((total - 6.0).abs() < 0.0001, "got {total}");
    }

    #[test]
    fn pricing_with_cache_explicit_rates() {
        let p = Pricing::with_cache(3.00, 15.00, 0.30, 3.75);
        assert_eq!(p.cache_read_per_mtok, Some(0.30));
        assert_eq!(p.cache_write_per_mtok, Some(3.75));
    }

    #[test]
    fn pricing_cost_for_matches_estimate_cost_usd() {
        // Per polish-9 + pass-10 NIT #1: Pricing::cost_for should
        // produce identical results to estimate_cost_usd on EVERY
        // path where the two could diverge — including the cache
        // and reasoning token paths, and when usage.cost_usd is
        // already populated by the provider (both should ignore
        // it when the model is in the registry).
        let mut r = CostRegistry::empty();
        let p = Pricing::with_cache(2.0, 8.0, 0.20, 2.50);
        r.override_for("test-model", p);

        let cases: Vec<(&str, Usage)> = vec![
            ("baseline", usage(1_000_000, 500_000)),
            (
                "with_cache_read",
                Usage { cache_read_tokens: 500_000, ..usage(500_000, 500_000) },
            ),
            (
                "with_cache_write",
                Usage { cache_write_tokens: 250_000, ..usage(500_000, 500_000) },
            ),
            (
                "with_reasoning",
                Usage { reasoning_tokens: 100_000, ..usage(500_000, 400_000) },
            ),
            (
                "with_cost_usd_already_populated",
                Usage { cost_usd: 99.99, ..usage(1_000_000, 500_000) },
            ),
        ];

        for (name, u) in cases {
            let via_registry = estimate_cost_usd(&u, "test-model", &r);
            let via_pricing = p.cost_for(&u);
            assert!(
                (via_registry - via_pricing).abs() < 1e-9,
                "case {name}: registry={via_registry} pricing={via_pricing}"
            );
        }
    }

    #[test]
    fn pricing_cost_for_handles_cache_rates() {
        // 1M input @ $3, 500k cache_read @ $0.30 (= $0.15),
        // 100k output @ $15 (= $1.50). Total: $4.65.
        let p = Pricing::with_cache(3.00, 15.00, 0.30, 3.75);
        let mut u = usage(1_000_000, 100_000);
        u.cache_read_tokens = 500_000;
        let cost = p.cost_for(&u);
        assert!((cost - 4.65).abs() < 0.0001, "got {cost}");
    }
}
