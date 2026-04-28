//! SQLite schema. Idempotent CREATEs + a `schema_version` table.
//! DDL is taken verbatim from RFD 0004.

use rusqlite::Connection;

pub const CURRENT_VERSION: i64 = 1;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
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
        "#,
    )?;
    Ok(())
}
