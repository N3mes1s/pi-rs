//! Test 1 from RFD 0004 §Test plan: ingest a tempdir JSONL with one
//! Meta + one Assistant + one Usage; assert exactly one row with the
//! right cost/folder/model. Re-run, assert idempotency.

use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use pi_ai::{ContentBlock, Message, Role, Usage};
use pi_stats::{ingest, open_in_memory};
use std::fs;
use std::io::Write;

fn line(e: &SessionEntry) -> String {
    let mut s = serde_json::to_string(e).unwrap();
    s.push('\n');
    s
}

fn meta_entry() -> SessionEntry {
    SessionEntry {
        id: "m1".into(),
        parent_id: None,
        timestamp: 1_700_000_000_000,
        kind: SessionEntryKind::Meta {
            cwd: "/work/proj".into(),
            provider: "anthropic".into(),
            model: "sonnet".into(),
            title: None,
        },
    }
}

fn assistant_entry(id: &str, ts: i64) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: Some("m1".into()),
        timestamp: ts,
        kind: SessionEntryKind::Assistant {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "hi".into(),
                }],
            },
        },
    }
}

fn usage_entry(id: &str, ts: i64, cost: f64) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::Usage {
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 10,
                cache_write_tokens: 5,
                reasoning_tokens: 0,
                cost_usd: cost,
            },
        },
    }
}

#[test]
fn ingest_meta_assistant_usage_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();
    let path = cwd_dir.join("abc.jsonl");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(line(&meta_entry()).as_bytes()).unwrap();
    f.write_all(line(&assistant_entry("a1", 1_700_000_001_000)).as_bytes())
        .unwrap();
    f.write_all(line(&usage_entry("u1", 1_700_000_001_500, 0.0125)).as_bytes())
        .unwrap();
    drop(f);

    let mut conn = open_in_memory().unwrap();
    let r1 = ingest::sync_all(&mut conn, &sessions).unwrap();
    assert_eq!(r1.rows, 1, "first sync should insert one row");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let (folder, model, provider, cost, input): (String, String, String, f64, i64) = conn
        .query_row(
            "SELECT folder, model, provider, cost_usd, input_tokens FROM messages",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(folder, "/work/proj");
    assert_eq!(model, "sonnet");
    assert_eq!(provider, "anthropic");
    assert!((cost - 0.0125).abs() < 1e-9);
    assert_eq!(input, 100);

    // Idempotent: a second sync should insert zero rows.
    let r2 = ingest::sync_all(&mut conn, &sessions).unwrap();
    assert_eq!(r2.rows, 0, "second sync should be a no-op");
    let count2: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count2, 1);
}

#[test]
fn ingest_partial_picks_up_appended_rows() {
    // Test 2 from RFD 0004 §Test plan: append more rows after a sync,
    // confirm the second sync ingests only the new ones.
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();
    let path = cwd_dir.join("abc.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(line(&meta_entry()).as_bytes()).unwrap();
        f.write_all(line(&assistant_entry("a1", 1_700_000_001_000)).as_bytes())
            .unwrap();
        f.write_all(line(&usage_entry("u1", 1_700_000_001_500, 0.01)).as_bytes())
            .unwrap();
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();
    let n1: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n1, 1);

    // Make sure mtime advances past 1ms granularity on slow filesystems.
    std::thread::sleep(std::time::Duration::from_millis(20));

    {
        use std::fs::OpenOptions;
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(line(&assistant_entry("a2", 1_700_000_002_000)).as_bytes())
            .unwrap();
        f.write_all(line(&usage_entry("u2", 1_700_000_002_500, 0.02)).as_bytes())
            .unwrap();
    }

    let r2 = ingest::sync_all(&mut conn, &sessions).unwrap();
    let n2: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n2, 2, "second sync should add the new assistant row");
    assert_eq!(r2.rows, 1, "only the appended assistant row is new");
}
