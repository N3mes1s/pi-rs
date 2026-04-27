//! `pi-stats` — ingest, aggregate, and serve pi-rs session statistics.
//!
//! Implements RFD 0004. Public surface:
//!
//! * [`open_db`] — open or create the SQLite database at the given path,
//!   apply pragmas, run schema migrations.
//! * [`ingest::sync_all`] — walk a sessions root directory and pull new
//!   rows out of every `*.jsonl` file (idempotent).
//! * [`aggregate::dashboard`] — produce the JSON payload backing
//!   `/api/stats`.
//! * [`server::router`] — build the axum router serving the dashboard.
//! * [`cli`] — `pi --stats {server,sync,json}` glue.

pub mod aggregate;
pub mod cli;
pub mod ingest;
pub mod schema;
pub mod server;

use rusqlite::Connection;
use std::path::Path;

/// Open (or create) the stats database at `path`, apply the standard
/// pragmas, and ensure the schema is materialised.
pub fn open_db(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    schema::ensure(&conn)?;
    Ok(conn)
}

/// Open an in-memory database — primarily for tests.
pub fn open_in_memory() -> anyhow::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    apply_pragmas(&conn)?;
    schema::ensure(&conn)?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    // WAL doesn't apply to in-memory DBs; ignore failures here.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}
