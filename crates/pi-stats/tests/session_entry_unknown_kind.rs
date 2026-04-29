//! Forward-compatibility invariant (RFD 0020 v1.1 §M3): when pi-stats
//! ingests a JSONL line whose `kind` is unknown to this version of the
//! enum (because the writer is on a newer pi-rs), the line must be
//! silently skipped — never fail the whole sync. This pins that
//! contract so a future enum hand-roll can't quietly turn it into a
//! fail-fast error.

use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use pi_ai::{ContentBlock, Message, Role, Usage};
use pi_stats::{ingest, open_in_memory};
use std::fs;
use std::io::Write;

fn jsonl(e: &SessionEntry) -> String {
    let mut s = serde_json::to_string(e).unwrap();
    s.push('\n');
    s
}

#[test]
fn ingest_skips_unknown_kind_lines_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();
    let path = cwd_dir.join("session.jsonl");

    let meta = SessionEntry {
        id: "m1".into(),
        parent_id: None,
        timestamp: 1_700_000_000_000,
        kind: SessionEntryKind::Meta {
            cwd: "/work/proj".into(),
            provider: "anthropic".into(),
            model: "sonnet".into(),
            title: None,
        },
    };
    let assistant = SessionEntry {
        id: "a1".into(),
        parent_id: Some("m1".into()),
        timestamp: 1_700_000_001_000,
        kind: SessionEntryKind::Assistant {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
        },
    };
    let usage = SessionEntry {
        id: "u1".into(),
        parent_id: None,
        timestamp: 1_700_000_001_500,
        kind: SessionEntryKind::Usage {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: 0.001,
            },
        },
    };

    // A line in the JSONL that *this* pi-stats build can't deserialise
    // — modelled as a future SessionEntryKind tag we don't recognise.
    let unknown_line = r#"{"id":"x1","parent_id":null,"timestamp":1700000000200,"kind":"future_kind_introduced_after_v1","payload":{"thing":42}}"#;

    {
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(jsonl(&meta).as_bytes()).unwrap();
        // The unknown line is sandwiched between known entries to prove
        // it doesn't poison the rest of the file's ingest.
        f.write_all(unknown_line.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        f.write_all(jsonl(&assistant).as_bytes()).unwrap();
        f.write_all(jsonl(&usage).as_bytes()).unwrap();
    }

    let mut conn = open_in_memory().unwrap();
    let report =
        ingest::sync_all(&mut conn, &sessions).expect("sync must not error on unknown kind");
    assert_eq!(report.files, 1);
    // One assistant row landed; the unknown line contributed nothing
    // and didn't abort the loop.
    assert_eq!(report.rows, 1);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
