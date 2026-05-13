//! RFD 0022 phase 4 dogfood: `pi --stats sandbox-actions` returns a non-empty
//! per-provider rollup after a session has run through the sandbox boundary.
//!
//! This mirrors the helper patterns in `sandbox_actions_aggregation.rs` and
//! exercises both the aggregation layer and the CLI formatter end-to-end
//! (in-process, without spawning a subprocess).

use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use pi_stats::{aggregate, cli, ingest, open_in_memory};
use std::fs;
use std::io::Write;

// ---------------------------------------------------------------------------
// Helpers (copied verbatim from sandbox_actions_aggregation.rs so this test
// is self-contained).
// ---------------------------------------------------------------------------

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
            cwd: "/work/dogfood".into(),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Write two SandboxAction entries for "local-process", sync into an
/// in-memory DB, and assert that `by_sandbox_provider` returns at least one
/// row with executions > 0.
#[test]
fn sandbox_actions_aggregation_returns_non_empty_rollup() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_dogfood");
    fs::create_dir_all(&cwd_dir).unwrap();

    let path = cwd_dir.join("dogfood.jsonl");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(line(&meta("m1", 1_700_000_000_000)).as_bytes())
        .unwrap();
    f.write_all(
        line(&action(
            "a1",
            1_700_000_000_100,
            "local-process",
            "bash",
            42,
            0,
        ))
        .as_bytes(),
    )
    .unwrap();
    f.write_all(
        line(&action(
            "a2",
            1_700_000_000_200,
            "local-process",
            "read",
            18,
            1, // non-zero → error
        ))
        .as_bytes(),
    )
    .unwrap();
    drop(f);

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let stats = aggregate::by_sandbox_provider(&conn).unwrap();
    assert!(
        !stats.is_empty(),
        "expected at least one provider row, got none"
    );
    let local = stats
        .iter()
        .find(|r| r.provider == "local-process")
        .expect("expected a 'local-process' row");
    assert!(
        local.executions > 0,
        "executions should be > 0, got {}",
        local.executions
    );
}

/// Full CLI-formatter path: synthesise entries → ingest → render via
/// `cli::render_sandbox_actions` and assert the output contains the
/// provider name and a non-zero executions count.
#[test]
fn cli_formatter_contains_provider_and_nonzero_executions() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let cwd_dir = sessions.join("_work_dogfood");
    fs::create_dir_all(&cwd_dir).unwrap();

    let path = cwd_dir.join("dogfood2.jsonl");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(line(&meta("m2", 1_700_000_001_000)).as_bytes())
        .unwrap();
    f.write_all(
        line(&action(
            "b1",
            1_700_000_001_100,
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
            "b2",
            1_700_000_001_200,
            "local-process",
            "write",
            25,
            0,
        ))
        .as_bytes(),
    )
    .unwrap();
    drop(f);

    let mut conn = open_in_memory().unwrap();
    ingest::sync_all(&mut conn, &sessions).unwrap();

    let mut stats = aggregate::by_sandbox_provider(&conn).unwrap();
    // Mirror the sort done by the `run` function.
    stats.sort_by(|a, b| a.provider.cmp(&b.provider));

    let output = cli::render_sandbox_actions(&stats);

    // The output must mention the provider name.
    assert!(
        output.contains("local-process"),
        "output should contain 'local-process':\n{output}"
    );

    // The executions column for local-process must be non-zero.
    // The table is fixed-width; find the local-process line and confirm
    // that its executions field (second column) is not "0".
    let lp_line = output
        .lines()
        .find(|l| l.contains("local-process"))
        .expect("no local-process line in output");

    // The executions count is the second whitespace-delimited token.
    let mut fields = lp_line.split_whitespace();
    let _provider = fields.next().unwrap();
    let executions: u64 = fields
        .next()
        .unwrap()
        .parse()
        .expect("second field should be a number");
    assert!(
        executions > 0,
        "executions column should be > 0, got {executions}"
    );
}
