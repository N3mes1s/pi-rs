//! `state.jsonl` event-envelope writer — RFD 0025 §State event schema.
//!
//! Every step or meta-event halo emits is a single JSON line appended to
//! `~/.pi/halo/<repo>/state.jsonl`. This module provides the typed
//! helpers that construct and persist those events.

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Append a single event JSON-line to `state.jsonl`.
pub fn append_event(state_jsonl: &Path, event: &Value) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_jsonl)?;
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}

fn now_ts() -> String {
    Utc::now().to_rfc3339()
}

/// Append a `kind:"step"` event — represents one step terminal.
pub fn append_step(
    state_jsonl: &Path,
    cycle: u64,
    step: &str,
    status: &str,
    detail: Value,
) -> Result<()> {
    let evt = json!({
        "kind": "step",
        "ts": now_ts(),
        "cycle": cycle,
        "step": step,
        "status": status,
        "detail": detail,
    });
    append_event(state_jsonl, &evt)
}

/// Append a `kind:"meta"` event (e.g. `CYCLE_DONE`, `CYCLE_ABORTED`,
/// `STREAK_RESET`, `STREAK_INCREMENTED`, `STALE_DISPATCHED_RECOVERED`).
pub fn append_meta(state_jsonl: &Path, meta: &str, detail: Value) -> Result<()> {
    let evt = json!({
        "kind": "meta",
        "ts": now_ts(),
        "meta": meta,
        "detail": detail,
    });
    append_event(state_jsonl, &evt)
}

/// Append STEP_*_DONE (canonical helper).
pub fn step_done(state_jsonl: &Path, cycle: u64, step: &str, detail: Value) -> Result<()> {
    let status = format!("STEP_{}_DONE", step.to_uppercase());
    append_step(state_jsonl, cycle, step, &status, detail)
}

/// Append STEP_*_FAILED (canonical helper).
pub fn step_failed(state_jsonl: &Path, cycle: u64, step: &str, detail: Value) -> Result<()> {
    let status = format!("STEP_{}_FAILED", step.to_uppercase());
    append_step(state_jsonl, cycle, step, &status, detail)
}

/// Emit `meta:"CYCLE_DONE"`.
pub fn cycle_done(state_jsonl: &Path, cycle: u64, outcome: &str) -> Result<()> {
    append_meta(
        state_jsonl,
        "CYCLE_DONE",
        json!({"cycle": cycle, "outcome": outcome}),
    )
}

/// Emit `meta:"CYCLE_ABORTED"`.
pub fn cycle_aborted(state_jsonl: &Path, cycle: u64, detail: Value) -> Result<()> {
    append_meta(state_jsonl, "CYCLE_ABORTED", {
        let mut d = json!({"cycle": cycle});
        if let (Some(obj), Some(dobj)) = (d.as_object_mut(), detail.as_object()) {
            for (k, v) in dobj {
                obj.insert(k.clone(), v.clone());
            }
        }
        d
    })
}

/// Parse `state.jsonl` lines — returns the raw `serde_json::Value`
/// for each valid JSON line (invalid lines silently skipped).
pub fn parse_state_events(state_jsonl: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(state_jsonl) else {
        return vec![];
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Check whether `state.jsonl` contains a cycle terminal (`CYCLE_DONE` or
/// `CYCLE_ABORTED`) for the given cycle number.
pub fn has_cycle_terminal(events: &[Value], cycle: u64) -> bool {
    events.iter().any(|e| {
        let is_meta = e.get("kind").and_then(|v| v.as_str()) == Some("meta");
        if !is_meta {
            return false;
        }
        let meta = e.get("meta").and_then(|v| v.as_str());
        if !matches!(meta, Some("CYCLE_DONE") | Some("CYCLE_ABORTED")) {
            return false;
        }
        e.get("detail")
            .and_then(|d| d.get("cycle"))
            .and_then(|v| v.as_u64())
            == Some(cycle)
    })
}
