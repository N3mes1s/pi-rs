//! Cost computation helper (RFD 0008 + 0010).
//!
//! Lives outside `provider/anthropic.rs` so OpenAI / Google / Bedrock can
//! share it once they get their RFD-0008-style `Usage` population.
//!
//! Cache traffic is billed per-row when `cache_read_cost_per_mtok` /
//! `cache_write_cost_per_mtok` are populated; rows that omit them inherit
//! `input_cost_per_mtok` (the RFD-0008 fallback path).

use crate::provider::anthropic::UsageAcc;
use crate::registry::ModelInfo;

/// Compute USD cost for a streamed turn given its accumulated usage.
///
/// Falls back to the input rate when a row doesn't carry explicit cache
/// rates — byte-identical to the RFD-0008 behaviour.
pub fn compute_cost(model: &ModelInfo, u: &UsageAcc) -> f64 {
    let cache_read_rate = model
        .cache_read_cost_per_mtok
        .unwrap_or(model.input_cost_per_mtok);
    let cache_write_rate = model
        .cache_write_cost_per_mtok
        .unwrap_or(model.input_cost_per_mtok);
    let fresh_input = u.input_tokens;
    let cached_in = u.cache_read_tok;
    let cached_write = u.cache_write_tok;
    let out_tok = u.output_tokens + u.reasoning_tok;
    let in_dollars = (fresh_input as f64 / 1_000_000.0) * model.input_cost_per_mtok
        + (cached_in as f64 / 1_000_000.0) * cache_read_rate
        + (cached_write as f64 / 1_000_000.0) * cache_write_rate;
    let out_dollars = (out_tok as f64 / 1_000_000.0) * model.output_cost_per_mtok;
    in_dollars + out_dollars
}
