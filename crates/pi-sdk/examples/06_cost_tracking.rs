//! End-to-end cost tracking through pi-sdk's `cost` module.
//!
//! Demonstrates:
//! - `CostRegistry::with_bundled_defaults()` for the price table
//!   shipped with the SDK (best-effort list prices, override per
//!   contract pricing).
//! - `CostRegistry::override_for(model_id, Pricing)` for a custom
//!   negotiated rate.
//! - `Pricing::cost_for(usage)` for the registry-free hot path
//!   (compute many costs against the same Pricing).
//! - `estimate_cost_usd(usage, model_id, &registry)` for the
//!   one-shot path that's appropriate from inside an
//!   `AgentEvent::Usage` handler.
//! - `sum_session_cost_usd(usages_iter, model_id, &registry)` for
//!   per-session aggregation.
//!
//! Run with:
//!     cargo run --example 06_cost_tracking -p pi-sdk

use pi_sdk::{
    cost::{estimate_cost_usd, sum_session_cost_usd, CostRegistry, Pricing},
    Usage,
};

fn make_usage(input: u64, output: u64, cache_read: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        cost_usd: 0.0,
    }
}

fn main() {
    // 1. The bundled price table is the embedder's first stop.
    let mut registry = CostRegistry::with_bundled_defaults();
    println!(
        "[registry] bundled defaults cover {} models",
        registry.len()
    );

    // 2. Override the rate for a contract-priced model. Public
    //    list prices may not match what the embedder is actually
    //    paying.
    registry.override_for(
        "claude-haiku-4-5-20251001",
        Pricing::with_cache(0.50, 2.50, 0.05, 0.625),
    );
    println!("[registry] overrode claude-haiku-4-5 with contract pricing");

    // 3. One-shot via estimate_cost_usd (inside a Usage event handler).
    let turn1 = make_usage(100_000, 50_000, 0);
    let turn1_cost =
        estimate_cost_usd(&turn1, "claude-haiku-4-5-20251001", &registry);
    println!(
        "[turn 1] in={input} out={output} cost=${cost:.4}",
        input = turn1.input_tokens,
        output = turn1.output_tokens,
        cost = turn1_cost,
    );

    // 4. Multi-turn aggregation via sum_session_cost_usd.
    let session_usages = vec![
        make_usage(100_000, 50_000, 0),       // ~$0.175
        make_usage(50_000, 25_000, 50_000),   // ~$0.0900 (with cache)
        make_usage(75_000, 30_000, 25_000),   // ~$0.1138 (with cache)
    ];
    let session_total =
        sum_session_cost_usd(session_usages.iter(), "claude-haiku-4-5-20251001", &registry);
    println!("[session] {} turns, total=${session_total:.4}", session_usages.len());

    // 5. Hot loop via Pricing::cost_for (no registry indirection).
    //    Useful when the embedder has the Pricing in hand and is
    //    summing thousands of historical Usage events from a JSONL log.
    let pricing = registry
        .get("claude-haiku-4-5-20251001")
        .expect("just overrode this above");
    let hot_loop_total: f64 = session_usages.iter().map(|u| pricing.cost_for(u)).sum();
    println!(
        "[pricing] same total via Pricing::cost_for hot loop: ${hot_loop_total:.4} (matches: {})",
        (hot_loop_total - session_total).abs() < 1e-9
    );

    // 6. Embedders that want to know their bundled-default coverage
    //    can iterate the registry: `registry.len()` + look up by
    //    model_id. (The registry doesn't expose an iter() today —
    //    embedders building dashboards typically know the model_ids
    //    they care about and look them up directly.)
    for model_id in [
        "claude-opus-4-7",
        "claude-sonnet-4-6",
        "gpt-5",
        "o3",
        "gemini-2.5-pro",
        "model-not-bundled-yet",
    ] {
        match registry.get(model_id) {
            Some(p) => println!(
                "  {model_id:<32}  in=${in_:.2}/MTok out=${out_:.2}/MTok",
                in_ = p.input_per_mtok,
                out_ = p.output_per_mtok,
            ),
            None => println!("  {model_id:<32}  (not in bundled defaults)"),
        }
    }
}
