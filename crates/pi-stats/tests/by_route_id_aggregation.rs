//! pi-stats §RFD 0020 M3: aggregate `RoutingDecision` session entries
//! by `route_id`, with decision counts, distinct sessions, and an
//! average TALE-EP `<budget>` (telemetry-only on the `hard` route).

use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use pi_stats::{aggregate, ingest, open_in_memory};
use std::fs;
use std::io::Write;

fn line(e: &SessionEntry) -> String {
    let mut s = serde_json::to_string(e).unwrap();
    s.push('\n');
    s
}

fn meta(id: &str, ts: i64) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::Meta {
            cwd: "/work/proj".into(),
            provider: "anthropic".into(),
            model: "sonnet".into(),
            title: None,
        },
    }
}

fn route(id: &str, ts: i64, route_id: &str, budget: Option<u64>) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::RoutingDecision {
            route_id: route_id.into(),
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            thinking: "off".into(),
            budget_tokens: budget,
        },
    }
}

#[test]
fn by_route_id_groups_decisions_with_session_counts_and_avg_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    // Session A: 2 fast decisions, 1 hard decision with budget=200.
    let path_a = cwd_dir.join("aaa.jsonl");
    {
        let mut f = fs::File::create(&path_a).unwrap();
        f.write_all(line(&meta("ma", 1_700_000_000_000)).as_bytes()).unwrap();
        f.write_all(line(&route("r1", 1_700_000_000_100, "fast", None)).as_bytes()).unwrap();
        f.write_all(line(&route("r2", 1_700_000_000_200, "fast", None)).as_bytes()).unwrap();
        f.write_all(line(&route("r3", 1_700_000_000_300, "hard", Some(200))).as_bytes()).unwrap();
    }

    // Session B: 1 hard decision with budget=400 — distinct session_file.
    let path_b = cwd_dir.join("bbb.jsonl");
    {
        let mut f = fs::File::create(&path_b).unwrap();
        f.write_all(line(&meta("mb", 1_700_000_001_000)).as_bytes()).unwrap();
        f.write_all(line(&route("r4", 1_700_000_001_100, "hard", Some(400))).as_bytes()).unwrap();
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let mut stats = aggregate::by_route_id(&conn).unwrap();
    stats.sort_by(|a, b| a.route_id.cmp(&b.route_id));
    assert_eq!(stats.len(), 2, "expected 2 route groups, got {stats:?}");

    let fast = &stats[0];
    assert_eq!(fast.route_id, "fast");
    assert_eq!(fast.decisions, 2);
    assert_eq!(fast.sessions, 1);
    assert_eq!(fast.avg_budget_tokens, None);

    let hard = &stats[1];
    assert_eq!(hard.route_id, "hard");
    assert_eq!(hard.decisions, 2);
    assert_eq!(hard.sessions, 2);
    let avg = hard
        .avg_budget_tokens
        .expect("hard route should have an avg budget");
    assert!((avg - 300.0).abs() < 1e-9, "avg budget {avg} != 300");
}

#[test]
fn by_route_id_handles_no_routing_decisions() {
    let conn = open_in_memory().unwrap();
    let stats = aggregate::by_route_id(&conn).unwrap();
    assert!(stats.is_empty());
}

#[test]
fn by_route_id_groups_within_one_session_set_sessions_to_one_per_group() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    let path = cwd_dir.join("mixed.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(line(&meta("m1", 1_700_000_000_000)).as_bytes()).unwrap();
        f.write_all(line(&route("r1", 1_700_000_000_100, "fast", None)).as_bytes()).unwrap();
        f.write_all(line(&route("r2", 1_700_000_000_200, "fast", None)).as_bytes()).unwrap();
        f.write_all(line(&route("r3", 1_700_000_000_300, "default", None)).as_bytes()).unwrap();
        f.write_all(line(&route("r4", 1_700_000_000_400, "hard", None)).as_bytes()).unwrap();
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let stats = aggregate::by_route_id(&conn).unwrap();
    assert_eq!(stats.len(), 3, "expected 3 route groups, got {stats:?}");
    for s in &stats {
        assert_eq!(s.sessions, 1, "every group should report sessions=1: {s:?}");
    }
}

#[test]
fn by_route_id_aggregates_same_route_across_distinct_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    for (sess_idx, file_name) in ["a.jsonl", "b.jsonl", "c.jsonl"].iter().enumerate() {
        let path = cwd_dir.join(file_name);
        let mut f = fs::File::create(&path).unwrap();
        let base_ts = 1_700_000_000_000_i64 + (sess_idx as i64) * 10_000;
        f.write_all(line(&meta(&format!("m{sess_idx}"), base_ts)).as_bytes()).unwrap();
        for j in 0..5 {
            let id = format!("r{sess_idx}_{j}");
            let ts = base_ts + 100 + (j as i64) * 10;
            f.write_all(line(&route(&id, ts, "default", None)).as_bytes()).unwrap();
        }
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let stats = aggregate::by_route_id(&conn).unwrap();
    assert_eq!(stats.len(), 1);
    let s = &stats[0];
    assert_eq!(s.route_id, "default");
    assert_eq!(s.decisions, 15);
    assert_eq!(s.sessions, 3);
    assert_eq!(s.avg_budget_tokens, None);
}
