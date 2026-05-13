use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct StateEvent {
    pub kind: String,
    pub ts: Option<String>,
    pub meta: Option<String>,
    pub detail: Option<Value>,
}

pub fn replay_streak(state_jsonl: &Path) -> u32 {
    let mut streak = 0u32;
    for evt in parse_state_events(state_jsonl) {
        if evt.kind != "meta" {
            continue;
        }
        match evt.meta.as_deref() {
            Some("STREAK_RESET") => streak = 0,
            Some("STREAK_INCREMENTED") => streak += 1,
            _ => {}
        }
    }
    streak
}

pub fn commit_rate_60m(state_jsonl: &Path) -> u32 {
    let cutoff = Utc::now() - chrono::Duration::minutes(60);
    parse_state_events(state_jsonl)
        .into_iter()
        .filter(|evt| evt.kind == "meta" && evt.meta.as_deref() == Some("COMMIT_RECORDED"))
        .filter(|evt| {
            evt.ts
                .as_deref()
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|t| t.with_timezone(&Utc) >= cutoff)
                .unwrap_or(false)
        })
        .count() as u32
}

pub fn parse_state_events(state_jsonl: &Path) -> Vec<StateEvent> {
    let Ok(text) = std::fs::read_to_string(state_jsonl) else { return vec![]; };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}
