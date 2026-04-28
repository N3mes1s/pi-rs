//! §RFD 0020 M3 telemetry — `aggregate::route_savings`.
//!
//! Fixture: one session JSONL with three routing_decision/Assistant/Usage
//! triples (fast/haiku, default/sonnet, hard/gpt-5.4). After sync, verifies
//! per-route counts and counterfactual pricing.

use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use pi_ai::{ContentBlock, Message, Role, Usage};
use pi_stats::{aggregate, ingest, open_in_memory};
use std::fs;
use std::io::Write;

fn line(e: &SessionEntry) -> String {
    let mut s = serde_json::to_string(e).unwrap();
    s.push('\n');
    s
}

fn meta_entry(id: &str, ts: i64) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::Meta {
            cwd: "/work/proj".into(),
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            title: None,
        },
    }
}

fn route_entry(id: &str, ts: i64, route_id: &str, provider: &str, model: &str) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::RoutingDecision {
            route_id: route_id.into(),
            provider: provider.into(),
            model: model.into(),
            thinking: "off".into(),
            budget_tokens: None,
        },
    }
}

fn assistant_entry(id: &str, ts: i64) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::Assistant {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "response".into(),
                }],
            },
        },
    }
}

fn usage_entry(
    id: &str,
    ts: i64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::Usage {
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
                reasoning_tokens: 0,
                cost_usd: cost,
            },
        },
    }
}

/// Compute the sonnet counterfactual cost for a given token set.
/// $3.00/Mtok input, $15.00/Mtok output, $0.30/Mtok cache_read, $3.75/Mtok cache_write.
fn expected_sonnet_cost(input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
    let m = 1_000_000.0_f64;
    (input as f64) / m * 3.00
        + (output as f64) / m * 15.00
        + (cache_read as f64) / m * 0.30
        + (cache_write as f64) / m * 3.75
}

#[test]
fn route_savings_three_routes_one_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    // Token fixtures per turn:
    // Turn 1 (fast/haiku):    100 input, 50 output, 10 cache_read, 5 cache_write
    // Turn 2 (default/sonnet): 200 input, 80 output, 0 cache_read, 0 cache_write
    // Turn 3 (hard/gpt-5.4):  300 input, 120 output, 20 cache_read, 10 cache_write

    let path = cwd_dir.join("fixture.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();

        // Meta
        f.write_all(line(&meta_entry("m1", 1_000_000)).as_bytes()).unwrap();

        // Turn 1: fast / haiku
        let base1 = 1_000_100_i64;
        f.write_all(
            line(&route_entry("rd1", base1, "fast", "anthropic", "claude-haiku-4-5-20251001"))
                .as_bytes(),
        )
        .unwrap();
        f.write_all(line(&assistant_entry("a1", base1 + 50)).as_bytes()).unwrap();
        f.write_all(
            line(&usage_entry("u1", base1 + 100, 100, 50, 10, 5, 0.000_050)).as_bytes(),
        )
        .unwrap();

        // Turn 2: default / sonnet
        let base2 = 1_001_000_i64;
        f.write_all(
            line(&route_entry(
                "rd2",
                base2,
                "default",
                "anthropic",
                "claude-sonnet-4-6",
            ))
            .as_bytes(),
        )
        .unwrap();
        f.write_all(line(&assistant_entry("a2", base2 + 50)).as_bytes()).unwrap();
        f.write_all(
            line(&usage_entry("u2", base2 + 100, 200, 80, 0, 0, 0.003_200)).as_bytes(),
        )
        .unwrap();

        // Turn 3: hard / gpt-5.4
        let base3 = 1_002_000_i64;
        f.write_all(
            line(&route_entry("rd3", base3, "hard", "openai", "gpt-5.4")).as_bytes(),
        )
        .unwrap();
        f.write_all(line(&assistant_entry("a3", base3 + 50)).as_bytes()).unwrap();
        f.write_all(
            line(&usage_entry("u3", base3 + 100, 300, 120, 20, 10, 0.010_000)).as_bytes(),
        )
        .unwrap();
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let mut savings = aggregate::route_savings(&conn).unwrap();
    savings.sort_by(|a, b| a.route_id.cmp(&b.route_id));

    assert_eq!(savings.len(), 3, "expected 3 route rows, got {savings:?}");

    // Verify route_id grouping
    assert_eq!(savings[0].route_id, "default");
    assert_eq!(savings[1].route_id, "fast");
    assert_eq!(savings[2].route_id, "hard");

    // Each route has exactly 1 turn in this session
    assert_eq!(savings[0].turns, 1, "default: 1 turn");
    assert_eq!(savings[1].turns, 1, "fast: 1 turn");
    assert_eq!(savings[2].turns, 1, "hard: 1 turn");

    // Verify counterfactual cost for "fast" turn (100 in, 50 out, 10 cache_read, 5 cache_write)
    let expected_fast = expected_sonnet_cost(100, 50, 10, 5);
    assert!(
        (savings[1].counterfactual_cost_usd - expected_fast).abs() < 1e-10,
        "fast counterfactual: got {}, expected {}",
        savings[1].counterfactual_cost_usd,
        expected_fast
    );

    // Verify counterfactual cost for "default" turn (200 in, 80 out, 0, 0)
    let expected_default = expected_sonnet_cost(200, 80, 0, 0);
    assert!(
        (savings[0].counterfactual_cost_usd - expected_default).abs() < 1e-10,
        "default counterfactual: got {}, expected {}",
        savings[0].counterfactual_cost_usd,
        expected_default
    );

    // Verify counterfactual cost for "hard" turn (300 in, 120 out, 20, 10)
    let expected_hard = expected_sonnet_cost(300, 120, 20, 10);
    assert!(
        (savings[2].counterfactual_cost_usd - expected_hard).abs() < 1e-10,
        "hard counterfactual: got {}, expected {}",
        savings[2].counterfactual_cost_usd,
        expected_hard
    );

    // Spot-check actual costs (these come from the Usage rows we inserted)
    assert!(
        (savings[1].actual_cost_usd - 0.000_050).abs() < 1e-9,
        "fast actual cost mismatch"
    );
    assert!(
        (savings[0].actual_cost_usd - 0.003_200).abs() < 1e-9,
        "default actual cost mismatch"
    );
    assert!(
        (savings[2].actual_cost_usd - 0.010_000).abs() < 1e-9,
        "hard actual cost mismatch"
    );
}

#[test]
fn route_savings_skips_routing_decision_with_no_following_message() {
    // A routing decision with no subsequent assistant message should be omitted.
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    let path = cwd_dir.join("partial.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(line(&meta_entry("m1", 1_000_000)).as_bytes()).unwrap();

        // This routing decision has a matching assistant row.
        f.write_all(
            line(&route_entry("rd1", 1_001_000, "fast", "anthropic", "haiku"))
                .as_bytes(),
        )
        .unwrap();
        f.write_all(line(&assistant_entry("a1", 1_001_050)).as_bytes()).unwrap();
        f.write_all(
            line(&usage_entry("u1", 1_001_100, 100, 50, 0, 0, 0.001)).as_bytes(),
        )
        .unwrap();

        // This routing decision has no subsequent assistant row (turn errored / dropped).
        f.write_all(
            line(&route_entry("rd2", 1_002_000, "hard", "openai", "gpt-5")).as_bytes(),
        )
        .unwrap();
        // No assistant entry follows rd2.
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let savings = aggregate::route_savings(&conn).unwrap();
    assert_eq!(
        savings.len(),
        1,
        "only 'fast' should appear; 'hard' had no paired message: {savings:?}"
    );
    assert_eq!(savings[0].route_id, "fast");
}

#[test]
fn route_savings_empty_when_no_routing_decisions() {
    let conn = open_in_memory().unwrap();
    let savings = aggregate::route_savings(&conn).unwrap();
    assert!(savings.is_empty(), "expected empty vec with no data");
}
