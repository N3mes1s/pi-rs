//! SQLite schema. Idempotent CREATEs + a `schema_version` table.
//! DDL is taken verbatim from RFD 0004.

use rusqlite::Connection;

pub const CURRENT_VERSION: i64 = 2;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    // Step 1: run the baseline DDL (creates tables if they don't exist).
    // `INSERT OR IGNORE INTO schema_version VALUES (1)` runs on every call;
    // after migration the table holds rows {1, 2}. Use `SELECT MAX(version)`
    // so the gate is stable regardless of how many rows are present.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY
        );
        INSERT OR IGNORE INTO schema_version VALUES (1);

        CREATE TABLE IF NOT EXISTS messages (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_file    TEXT    NOT NULL,
            entry_id        TEXT    NOT NULL,
            folder          TEXT    NOT NULL,
            model           TEXT    NOT NULL,
            provider        TEXT    NOT NULL,
            timestamp_ms    INTEGER NOT NULL,
            duration_ms     INTEGER,
            ttft_ms         INTEGER,
            stop_reason     TEXT    NOT NULL,
            error_message   TEXT,
            input_tokens    INTEGER NOT NULL DEFAULT 0,
            output_tokens   INTEGER NOT NULL DEFAULT 0,
            cache_read_tok  INTEGER NOT NULL DEFAULT 0,
            cache_write_tok INTEGER NOT NULL DEFAULT 0,
            reasoning_tok   INTEGER NOT NULL DEFAULT 0,
            total_tokens    INTEGER NOT NULL DEFAULT 0,
            cost_usd        REAL    NOT NULL DEFAULT 0.0,
            UNIQUE (session_file, entry_id)
        );
        CREATE INDEX IF NOT EXISTS idx_messages_ts      ON messages(timestamp_ms);
        CREATE INDEX IF NOT EXISTS idx_messages_model   ON messages(model);
        CREATE INDEX IF NOT EXISTS idx_messages_folder  ON messages(folder);
        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_file);

        CREATE TABLE IF NOT EXISTS approval_decisions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_file    TEXT    NOT NULL,
            entry_id        TEXT    NOT NULL,
            folder          TEXT    NOT NULL,
            timestamp_ms    INTEGER NOT NULL,
            tool_name       TEXT    NOT NULL,
            mode            TEXT    NOT NULL,
            decision        TEXT    NOT NULL,
            reason          TEXT    NOT NULL,
            classifier_used INTEGER NOT NULL DEFAULT 0,
            classifier_risk TEXT,
            UNIQUE (session_file, entry_id)
        );
        CREATE INDEX IF NOT EXISTS idx_approvals_ts     ON approval_decisions(timestamp_ms);
        CREATE INDEX IF NOT EXISTS idx_approvals_tool   ON approval_decisions(tool_name);

        CREATE TABLE IF NOT EXISTS file_offsets (
            session_file       TEXT PRIMARY KEY,
            byte_offset        INTEGER NOT NULL,
            last_modified_ms   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS routing_decisions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_file    TEXT    NOT NULL,
            entry_id        TEXT    NOT NULL,
            folder          TEXT    NOT NULL,
            timestamp_ms    INTEGER NOT NULL,
            route_id        TEXT    NOT NULL,
            provider        TEXT    NOT NULL,
            model           TEXT    NOT NULL,
            thinking        TEXT    NOT NULL,
            budget_tokens   INTEGER,
            UNIQUE (session_file, entry_id)
        );
        CREATE INDEX IF NOT EXISTS idx_routing_route_id ON routing_decisions(route_id);
        CREATE INDEX IF NOT EXISTS idx_routing_ts       ON routing_decisions(timestamp_ms);

        CREATE TABLE IF NOT EXISTS sandbox_actions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_file    TEXT    NOT NULL,
            entry_id        TEXT    NOT NULL,
            folder          TEXT    NOT NULL,
            timestamp_ms    INTEGER NOT NULL,
            provider        TEXT    NOT NULL,
            tool_name       TEXT    NOT NULL,
            duration_ms     INTEGER NOT NULL,
            exit_status     INTEGER NOT NULL,
            is_error        INTEGER NOT NULL DEFAULT 0,
            UNIQUE (session_file, entry_id)
        );
        CREATE INDEX IF NOT EXISTS idx_sandbox_provider  ON sandbox_actions(provider);
        CREATE INDEX IF NOT EXISTS idx_sandbox_tool_name ON sandbox_actions(tool_name);
        CREATE INDEX IF NOT EXISTS idx_sandbox_ts        ON sandbox_actions(timestamp_ms);
        "#,
    )?;

    // Step 2: apply incremental migrations by version.
    // Use MAX(version) so the result is stable even when the bootstrap
    // INSERT OR IGNORE re-inserts row 1 on each call.
    let version: i64 = conn
        .query_row(
            "SELECT MAX(version) FROM schema_version",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .unwrap_or(None)
        .unwrap_or(0);

    if version < 2 {
        // SQLite ALTER TABLE ADD COLUMN is not idempotent; the `version < 2`
        // gate ensures this block runs exactly once per database.
        // Adds cost_usd and round_trip_ms to sandbox_actions (RFD 0026).
        conn.execute_batch(
            r#"
            ALTER TABLE sandbox_actions ADD COLUMN cost_usd      REAL;
            ALTER TABLE sandbox_actions ADD COLUMN round_trip_ms INTEGER;
            INSERT OR REPLACE INTO schema_version VALUES (2);
            "#,
        )?;
    }

    Ok(())
}
