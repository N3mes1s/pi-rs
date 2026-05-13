//! Halo spend ledger writer — RFD 0025 §Spend accounting.
//!
//! Appends one `usage.jsonl` row per cost-bearing sub-operation. v1 rows
//! are always `exact: false` with a documented `estimate_basis`.

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

fn now_ts() -> String {
    Utc::now().to_rfc3339()
}

fn append_row(usage_jsonl: &Path, row: Value) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(usage_jsonl)?;
    let mut line = serde_json::to_string(&row)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Write an `evolve_tick` spend row.
///
/// `cost_usd` is sourced from the evolve `CostLedger` (candidate-benchmark
/// cost only — baseline + mutator-LLM costs are not counted in v1).
pub fn write_evolve_tick_row(
    usage_jsonl: &Path,
    cycle: u64,
    cost_usd: f64,
) -> Result<()> {
    append_row(
        usage_jsonl,
        json!({
            "kind": "evolve_tick",
            "cycle": cycle,
            "ts": now_ts(),
            "cost_usd": cost_usd,
            "exact": false,
            "estimate_basis": "evolve_candidates_only",
        }),
    )
}

/// Write an `orchestrate` spend row.
///
/// `elapsed_minutes` is `(orchestrate_exit_ts - cycle_start_ts) / 60.0`.
/// `budget_per_minute` is `[orchestrate].budget_dollars_per_minute_estimate`.
pub fn write_orchestrate_row(
    usage_jsonl: &Path,
    cycle: u64,
    elapsed_minutes: f64,
    budget_per_minute: f64,
) -> Result<()> {
    let cost_usd = elapsed_minutes * budget_per_minute;
    append_row(
        usage_jsonl,
        json!({
            "kind": "orchestrate",
            "cycle": cycle,
            "ts": now_ts(),
            "cost_usd": cost_usd,
            "exact": false,
            "estimate_basis": format!("wall_clock_minutes:{:.2}", elapsed_minutes),
        }),
    )
}

/// Write a `proposer` spend row.
///
/// Uses the fixed `[proposer].estimated_cost_usd_per_call` override.
pub fn write_proposer_row(usage_jsonl: &Path, cycle: u64, estimated_cost_usd: f64) -> Result<()> {
    append_row(
        usage_jsonl,
        json!({
            "kind": "proposer",
            "cycle": cycle,
            "ts": now_ts(),
            "cost_usd": estimated_cost_usd,
            "exact": false,
            "proposer_cost_unknown": true,
        }),
    )
}

/// Sum today's spend from `usage.jsonl`.
///
/// Rows whose `supersedes` field is non-null are considered correction
/// rows and their cost is skipped (last-row-wins deduplication).
pub fn today_spend(usage_jsonl: &Path) -> f64 {
    let Ok(text) = std::fs::read_to_string(usage_jsonl) else {
        return 0.0;
    };
    let midnight = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();

    let mut total = 0.0_f64;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row): std::result::Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        // Skip correction rows.
        if row.get("supersedes").map_or(false, |v| !v.is_null()) {
            continue;
        }
        // Only today's rows.
        if let Some(ts_str) = row.get("ts").and_then(|v| v.as_str()) {
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                if t.with_timezone(&Utc) < midnight {
                    continue;
                }
            }
        }
        total += row.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    }
    total
}
