//! pi-stats §RFD 0022: ingest `SandboxAction` JSONL entries into the
//! `sandbox_actions` SQLite table and roll them up by provider, mirroring
//! the `RoutingDecision` round-trip in `by_route_id_aggregation.rs`.

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

fn action(
    id: &str,
    ts: i64,
    provider: &str,
    tool_name: &str,
    duration_ms: u64,
    exit_status: i32,
) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        parent_id: None,
        timestamp: ts,
        kind: SessionEntryKind::SandboxAction {
            provider: provider.into(),
            tool_name: tool_name.into(),
            duration_ms,
            exit_status,
            is_error: exit_status != 0,
        },
    }
}

#[test]
fn by_sandbox_provider_groups_executions_with_avg_duration_and_error_rate() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    // local-process: 3 calls, 1 error → error_rate 1/3, avg_ms = (10+20+30)/3 = 20.
    // docker:        2 calls, 0 errors → error_rate 0,    avg_ms = 100.
    let path = cwd_dir.join("a.jsonl");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(line(&meta("m", 1_700_000_000_000)).as_bytes())
        .unwrap();
    f.write_all(
        line(&action(
            "s1",
            1_700_000_000_100,
            "local-process",
            "ls",
            10,
            0,
        ))
        .as_bytes(),
    )
    .unwrap();
    f.write_all(
        line(&action(
            "s2",
            1_700_000_000_200,
            "local-process",
            "read",
            20,
            0,
        ))
        .as_bytes(),
    )
    .unwrap();
    f.write_all(
        line(&action(
            "s3",
            1_700_000_000_300,
            "local-process",
            "bash",
            30,
            42, // non-zero exit → counted as error
        ))
        .as_bytes(),
    )
    .unwrap();
    f.write_all(
        line(&action(
            "s4",
            1_700_000_000_400,
            "docker",
            "ls",
            100,
            0,
        ))
        .as_bytes(),
    )
    .unwrap();
    f.write_all(
        line(&action(
            "s5",
            1_700_000_000_500,
            "docker",
            "read",
            100,
            0,
        ))
        .as_bytes(),
    )
    .unwrap();
    drop(f);

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let mut stats = aggregate::by_sandbox_provider(&conn).unwrap();
    stats.sort_by(|a, b| a.provider.cmp(&b.provider));
    assert_eq!(stats.len(), 2, "expected 2 provider groups, got {stats:?}");

    let docker = &stats[0];
    assert_eq!(docker.provider, "docker");
    assert_eq!(docker.executions, 2);
    assert_eq!(docker.errors, 0);
    assert_eq!(docker.error_rate, 0.0);
    assert!((docker.avg_duration_ms - 100.0).abs() < 1e-9);

    let local = &stats[1];
    assert_eq!(local.provider, "local-process");
    assert_eq!(local.executions, 3);
    assert_eq!(local.errors, 1);
    assert!(
        (local.error_rate - (1.0 / 3.0)).abs() < 1e-9,
        "got {}",
        local.error_rate
    );
    assert!((local.avg_duration_ms - 20.0).abs() < 1e-9);
}

#[test]
fn by_sandbox_provider_handles_no_actions() {
    let conn = open_in_memory().unwrap();
    let stats = aggregate::by_sandbox_provider(&conn).unwrap();
    assert!(stats.is_empty());
}

#[test]
fn ingest_idempotent_on_duplicate_sandbox_action_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_proj");
    fs::create_dir_all(&cwd_dir).unwrap();

    let path = cwd_dir.join("a.jsonl");
    {
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(line(&meta("m", 1_700_000_000_000)).as_bytes())
            .unwrap();
        f.write_all(
            line(&action(
                "s1",
                1_700_000_000_100,
                "local-process",
                "ls",
                10,
                0,
            ))
            .as_bytes(),
        )
        .unwrap();
    }

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();
    // Second sync over the same file must be a no-op (UNIQUE on
    // session_file+entry_id, guarded with INSERT OR IGNORE).
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let stats = aggregate::by_sandbox_provider(&conn).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].executions, 1);
}
