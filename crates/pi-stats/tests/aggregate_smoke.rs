//! Test 3 from RFD 0004 §Test plan: seed three models × four sessions
//! into an in-memory DB and confirm dashboard aggregation is sane.

use pi_stats::{aggregate, open_in_memory};
use rusqlite::params;

fn seed_row(
    conn: &rusqlite::Connection,
    session: &str,
    entry: &str,
    model: &str,
    folder: &str,
    cost: f64,
    input: i64,
    output: i64,
) {
    conn.execute(
        "INSERT INTO messages (
            session_file, entry_id, folder, model, provider,
            timestamp_ms, stop_reason,
            input_tokens, output_tokens, cache_read_tok, cache_write_tok,
            reasoning_tok, total_tokens, cost_usd
         ) VALUES (?1,?2,?3,?4,'anthropic',?5,'stop',?6,?7,0,0,0,?8,?9)",
        params![
            session,
            entry,
            folder,
            model,
            chrono::Utc::now().timestamp_millis(),
            input,
            output,
            input + output,
            cost,
        ],
    )
    .unwrap();
}

#[test]
fn dashboard_aggregates_three_models_four_sessions() {
    let conn = open_in_memory().unwrap();
    let models = ["sonnet", "haiku", "opus"];
    let sessions = ["s1", "s2", "s3", "s4"];
    let mut total = 0.0;
    let mut entry_id = 0;
    for m in models {
        for s in sessions {
            entry_id += 1;
            let cost = 0.001 * entry_id as f64;
            total += cost;
            seed_row(
                &conn,
                &format!("/sessions/{s}.jsonl"),
                &format!("e{entry_id}"),
                m,
                &format!("/proj/{s}"),
                cost,
                100,
                50,
            );
        }
    }

    let d = aggregate::dashboard(&conn).unwrap();
    assert_eq!(d.by_model.len(), 3);
    assert_eq!(d.overall.total_requests, 12);
    // 4 distinct session_file values across 3 models = 4 sessions overall.
    assert_eq!(d.overall.total_sessions, 4);
    // Each model touched all 4 sessions.
    for m in &d.by_model {
        assert_eq!(m.sessions, 4, "{} sessions", m.model);
    }
    assert!((d.overall.total_cost - total).abs() < 1e-9);
    // Folder breakdown: 4 distinct folders.
    assert_eq!(d.by_folder.len(), 4);
    // Sum-of-by_model cost ≈ overall.
    let sum: f64 = d.by_model.iter().map(|m| m.cost).sum();
    assert!((sum - d.overall.total_cost).abs() < 1e-9);
}
