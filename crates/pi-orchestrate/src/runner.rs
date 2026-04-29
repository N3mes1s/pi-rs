//! Sequential orchestrator runtime — v0 scaffolding.
//!
//! Walks the topologically-ordered campaign milestones, emits state
//! transitions to `<state_dir>/state.jsonl`, and reports a summary.
//! v0 does NOT actually spawn implementer/reviewer pis — each milestone
//! is logged as `dispatched` then immediately `stubbed_complete`. This
//! lets the runtime's CLI surface, persistence layout, and topological
//! walk be exercised end-to-end before the real spawn primitive lands
//! in v1 (which will swap `dispatch_stub` for an actual call to
//! `pi-coding-agent::native::task::executor::run_one`).
//!
//! State events are append-only one-per-line JSON, matching RFD 0021
//! §"Persisted state layout":
//!   {"milestone": "<id>", "from": "<state>", "to": "<state>",
//!    "ts": <unix-ms>, "detail": "..."}
//!
//! Milestone state machine in v0:
//!   PENDING → DISPATCHED → STUBBED_COMPLETE
//!
//! v1 will introduce REVIEWED / NEEDS_FIX / MERGED / FAILED transitions.

use crate::plan::topological_order;
use crate::schema::Campaign;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One state-transition event, persisted as a single line in `state.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateEvent {
    pub milestone: String,
    pub from: String,
    pub to: String,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Outcome of one milestone's stub dispatch.
#[derive(Debug, Clone)]
pub struct MilestoneOutcome {
    pub id: String,
    pub final_state: String,
    pub events_written: usize,
}

/// Aggregate summary of a `run` invocation.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub campaign: String,
    pub state_path: PathBuf,
    pub outcomes: Vec<MilestoneOutcome>,
}

/// Resolve `<state_root>/<campaign-name>/state.jsonl`, creating
/// the parent directory if needed. The campaign name is sanitised
/// (`/` → `_`) so a TOML name with slashes can't escape the root.
pub fn state_path_for(state_root: &Path, campaign_name: &str) -> std::io::Result<PathBuf> {
    let safe = campaign_name.replace(['/', '\\'], "_");
    let dir = state_root.join(&safe);
    fs::create_dir_all(&dir)?;
    Ok(dir.join("state.jsonl"))
}

/// Run the campaign's stub executor: emit DISPATCHED → STUBBED_COMPLETE
/// transitions for each milestone in topological order.
///
/// Returns a `RunSummary` listing each milestone's terminal state.
/// Errors only on I/O (cannot create state dir, cannot write events).
pub fn run(campaign: &Campaign, state_root: &Path) -> std::io::Result<RunSummary> {
    let state_path = state_path_for(state_root, &campaign.name)?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state_path)?;

    let mut outcomes = Vec::with_capacity(campaign.milestones.len());

    for (idx, m) in topological_order(campaign).iter().enumerate() {
        let total = campaign.milestones.len();
        println!(
            "[{}/{}] {} (implementer={} branch={})",
            idx + 1,
            total,
            m.id,
            m.implementer,
            m.branch,
        );
        let preview = m
            .assignment
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        println!("       assignment: {preview}");
        println!("       [v0 stub: real spawn lands in v1; logging events]");

        let dispatch_evt = StateEvent {
            milestone: m.id.clone(),
            from: "PENDING".into(),
            to: "DISPATCHED".into(),
            ts: now_ms(),
            detail: format!("implementer={} branch={}", m.implementer, m.branch),
        };
        write_event(&mut log, &dispatch_evt)?;

        let complete_evt = StateEvent {
            milestone: m.id.clone(),
            from: "DISPATCHED".into(),
            to: "STUBBED_COMPLETE".into(),
            ts: now_ms(),
            detail: "v0-stub".into(),
        };
        write_event(&mut log, &complete_evt)?;

        outcomes.push(MilestoneOutcome {
            id: m.id.clone(),
            final_state: "STUBBED_COMPLETE".into(),
            events_written: 2,
        });
    }

    Ok(RunSummary {
        campaign: campaign.name.clone(),
        state_path,
        outcomes,
    })
}

/// Serialise one event as a single line with a trailing newline.
/// Append-only so a partial write at most truncates the trailing
/// newline of the LAST event — replay is robust to that.
fn write_event(log: &mut std::fs::File, evt: &StateEvent) -> std::io::Result<()> {
    let line = serde_json::to_string(evt)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    log.write_all(line.as_bytes())?;
    log.write_all(b"\n")?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Replay an existing `state.jsonl` into the in-memory state map.
/// Skip a truncated trailing line (RFD 0021 §"Persisted state layout":
/// a partial write must not corrupt the snapshot).
pub fn replay(state_path: &Path) -> std::io::Result<Vec<StateEvent>> {
    let text = match fs::read_to_string(state_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut events = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<StateEvent>(line) {
            Ok(e) => events.push(e),
            Err(_) => {
                // Truncated final line: stop replay here without erroring.
                // RFD §"Persisted state layout" — partial writes can
                // never corrupt state; resume drops the truncated line.
                break;
            }
        }
    }
    Ok(events)
}
