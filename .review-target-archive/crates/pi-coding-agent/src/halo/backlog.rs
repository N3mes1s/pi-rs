use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub id: String,
    pub title: String,
    pub rationale: String,
    pub files: Vec<String>,
    pub priority: f64,
    pub est_cost: f64,
    pub status: String,
    pub source: String,
    pub attempt_count: u32,
    pub dropped: bool,
}

pub fn replay(backlog_jsonl: &Path) -> BTreeMap<String, Proposal> {
    let Ok(text) = std::fs::read_to_string(backlog_jsonl) else { return BTreeMap::new(); };
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let evt: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => { warn!("backlog: unparseable line ({e}): {line}"); continue; }
        };
        let Some(kind) = evt.get("kind").and_then(|v| v.as_str()) else { warn!("backlog: line missing 'kind': {line}"); continue; };
        let Some(id) = evt.get("id").and_then(|v| v.as_str()) else { continue; };
        match kind {
            "proposal_created" => {
                let files = evt.get("files_touched").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                map.insert(id.to_string(), Proposal {
                    id: id.to_string(),
                    title: evt.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    rationale: evt.get("rationale").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    files,
                    priority: evt.get("priority").and_then(|v| v.as_f64()).unwrap_or(0.5),
                    est_cost: evt.get("est_cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    status: "pending".into(),
                    source: evt.get("source").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
                    attempt_count: 0,
                    dropped: false,
                });
            }
            "proposal_status_changed" => {
                let Some(entry) = map.get_mut(id) else { continue; };
                if let Some(status) = evt.get("status").and_then(|v| v.as_str()) {
                    entry.status = status.to_string();
                    if status == "dispatched" { entry.attempt_count += 1; }
                }
            }
            "proposal_dropped" => {
                if let Some(entry) = map.get_mut(id) {
                    entry.status = "dropped".into();
                    entry.dropped = true;
                }
            }
            other => warn!("backlog: unknown event kind '{other}' for id={id}; skipping (forward-compat)"),
        }
    }
    map
}

pub fn latest_dispatched_cycle(backlog_jsonl: &Path, id: &str) -> Option<u64> {
    let text = std::fs::read_to_string(backlog_jsonl).ok()?;
    let mut latest = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let evt: Value = serde_json::from_str(line).ok()?;
        if evt.get("kind").and_then(|v| v.as_str()) != Some("proposal_status_changed") { continue; }
        if evt.get("id").and_then(|v| v.as_str()) != Some(id) { continue; }
        if evt.get("status").and_then(|v| v.as_str()) != Some("dispatched") { continue; }
        let cycle = evt.get("detail").and_then(|v| v.get("cycle")).and_then(|v| v.as_u64())?;
        latest = Some(cycle);
    }
    latest
}

pub fn pending_proposals(map: &BTreeMap<String, Proposal>) -> Vec<&Proposal> {
    map.values().filter(|p| p.status == "pending" && !p.dropped).collect()
}

pub fn append_proposal_created(backlog_jsonl: &Path, id: &str, title: &str, rationale: &str, files: &[String], priority: f64, est_cost_usd: f64, source: &str) -> Result<()> {
    append_line(backlog_jsonl, &serde_json::json!({"kind":"proposal_created","ts":Utc::now().to_rfc3339(),"id":id,"title":title,"rationale":rationale,"files_touched":files,"priority":priority,"est_cost_usd":est_cost_usd,"source":source,"attempt_count":0}))
}

pub fn append_proposal_status_changed(backlog_jsonl: &Path, id: &str, status: &str, detail: Value) -> Result<()> {
    append_line(backlog_jsonl, &serde_json::json!({"kind":"proposal_status_changed","ts":Utc::now().to_rfc3339(),"id":id,"status":status,"detail":detail}))
}

pub fn append_proposal_dropped(backlog_jsonl: &Path, id: &str, operator: &str) -> Result<()> {
    append_line(backlog_jsonl, &serde_json::json!({"kind":"proposal_dropped","ts":Utc::now().to_rfc3339(),"id":id,"operator":operator}))
}

fn append_line(path: &Path, evt: &Value) -> Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_string(evt)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    Ok(())
}
