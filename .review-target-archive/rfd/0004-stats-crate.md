# RFD 0004 — `pi-stats` crate

- **Status:** Implemented
- **Author:** pi-rs maintainers
- **Created:** 2026-04-27
- **Implemented:** 3074b8c

## Summary

Add a new workspace crate `pi-stats` that ingests the JSONL session
files pi-rs already produces, persists per-request rows into a local
SQLite database (`~/.pi/agent/stats.db`), and exposes a small HTTP API
+ embedded React dashboard on `http://127.0.0.1:3847` mirroring the
oh-my-pi `omp-stats` package. A new `pi --stats` subcommand drives
sync, JSON dump, and server modes from the existing `pi` binary so
users don't have to install a second tool.

## Background

Pi-rs already records everything we need. `SessionEntry` (see
`crates/pi-agent-core/src/session.rs:11`) is a tagged-union JSONL line
with variants for `Meta`, `Assistant`, `ToolCall`, `ToolResult`,
`Usage`, `Outcome`, etc. Each `Usage` carries `input_tokens`,
`output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
`reasoning_tokens`, **and a precomputed `cost_usd`** (see
`crates/pi-ai/src/message.rs::Usage`). Sessions live at
`~/.pi/agent/sessions/<cwd-slug>/<id>.jsonl` (see
`crates/pi-agent-core/src/session.rs:192,234`).

What we don't have: any aggregation, any persistence beyond the JSONL
files, any way to answer "how much did I spend this week, on which
project, on which model, with what cache hit rate". Oh-my-pi solves
this in `packages/stats/*` (TypeScript / Bun / better-sqlite3 / React).
This RFD is the Rust port of that surface, scaled down to the bits
that pull weight on day one. The bigger UX goals — leaderboards,
context-window heatmaps, "$/PR" — are out of scope for now.

## Proposal

### Crate layout

```
crates/pi-stats/
├── Cargo.toml
├── src/
│   ├── lib.rs              # public surface: Stats, sync_all, etc.
│   ├── schema.rs           # SQL DDL + migration runner
│   ├── ingest.rs           # JSONL → MessageStat / ApprovalStat
│   ├── aggregate.rs        # SQL queries → DashboardStats
│   ├── server.rs           # axum router + handlers
│   └── cli.rs              # `pi --stats {sync,server,json}` glue
├── assets/
│   ├── index.html          # built React bundle (single page)
│   ├── index.js            # bundled JS, included via include_bytes!
│   └── styles.css
└── tests/
    ├── ingest_session_jsonl.rs
    ├── aggregate_smoke.rs
    └── server_routes.rs
```

`pi-stats` becomes a workspace member listed in
`Cargo.toml::[workspace.members]` and a path-dep of `pi-coding-agent`
(so the existing `pi` binary can call into it for `--stats`). No
public API is added to other crates.

### Dependencies (Cargo.toml)

```toml
[dependencies]
pi-ai = { workspace = true }                       # Usage, Message types
pi-agent-core = { workspace = true }               # SessionEntry parsing
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
tokio = { workspace = true }                       # for axum
chrono = { workspace = true }
axum = "0.7"                                       # HTTP framework
tower-http = { version = "0.5", features = ["cors", "trace"] }
rusqlite = { version = "0.31", features = ["bundled", "chrono"] }
# `bundled` ships SQLite as C source so musl static link Just Works;
# matches the pi build profile (relocation-model = "static").
tracing = { workspace = true }
walkdir = { workspace = true }
clap = { workspace = true }                        # only for the cli.rs subcommand parser

[dev-dependencies]
tempfile = { workspace = true }
tokio = { workspace = true, features = ["macros"] }
reqwest = { workspace = true }
proptest = { workspace = true }
```

**Crate choice rationale.** `rusqlite` over `sqlx`: the workload is
a single embedded DB with no network, no compile-time query checking
needed, and `sqlx` adds a tokio-postgres-style runtime story we don't
need. `axum` over hand-rolled hyper: 9 routes don't justify hand
rolling, and tower middleware (cors, trace) is free. No connection
pool — one writer thread (`Mutex<Connection>`) plus reads on the same
connection in WAL mode is enough for a localhost dashboard.

### Database

`~/.pi/agent/stats.db` (resolve via `pi_coding_agent::context::agent_dir()`).
Open with `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`.

Schema is materialised once at startup by `schema::ensure(&conn)`.
Idempotent CREATEs + a `schema_version` table for migrations.

```sql
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);
INSERT OR IGNORE INTO schema_version VALUES (1);

-- One row per assistant message (== one billable LLM request).
CREATE TABLE IF NOT EXISTS messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_file    TEXT    NOT NULL,
    entry_id        TEXT    NOT NULL,
    folder          TEXT    NOT NULL,            -- session.cwd
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

-- One row per auto-approve gate decision.
CREATE TABLE IF NOT EXISTS approval_decisions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_file    TEXT    NOT NULL,
    entry_id        TEXT    NOT NULL,
    folder          TEXT    NOT NULL,
    timestamp_ms    INTEGER NOT NULL,
    tool_name       TEXT    NOT NULL,
    mode            TEXT    NOT NULL,           -- ask | auto_policy | auto_judge | yolo
    decision        TEXT    NOT NULL,           -- allow | ask | deny
    reason          TEXT    NOT NULL,
    classifier_used INTEGER NOT NULL DEFAULT 0,
    classifier_risk TEXT,
    UNIQUE (session_file, entry_id)
);
CREATE INDEX IF NOT EXISTS idx_approvals_ts     ON approval_decisions(timestamp_ms);
CREATE INDEX IF NOT EXISTS idx_approvals_tool   ON approval_decisions(tool_name);

-- Resume watermark per session file. Idempotent ingest: re-running
-- sync only reads bytes past the stored offset, mtime guards against
-- file-rewrite drift.
CREATE TABLE IF NOT EXISTS file_offsets (
    session_file       TEXT PRIMARY KEY,
    byte_offset        INTEGER NOT NULL,
    last_modified_ms   INTEGER NOT NULL
);
```

### Ingest

```rust
// crates/pi-stats/src/ingest.rs
use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use rusqlite::{params, Connection};

pub fn sync_all(conn: &mut Connection, sessions_root: &Path) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    for entry in walkdir::WalkDir::new(sessions_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
    {
        let path = entry.path();
        let meta = std::fs::metadata(path)?;
        let mtime_ms = meta.modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as i64;
        let (offset, prev_mtime) = read_offset(conn, path)?;
        if mtime_ms < prev_mtime {
            // Clock skew or file truncation — re-ingest from scratch.
            reset_offset(conn, path)?;
        }
        let inserted = ingest_one(conn, path, offset)?;
        write_offset(conn, path, file_len(path)?, mtime_ms)?;
        report.files += 1;
        report.rows  += inserted;
    }
    Ok(report)
}

fn ingest_one(conn: &Connection, path: &Path, start: u64) -> Result<u64> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let reader = BufReader::new(f);

    // Pi-rs sessions carry the `Meta` line first; we cache it so each
    // assistant row knows model+provider+folder without re-walking.
    let mut session_meta: Option<MetaCache> = None;
    let mut inserted = 0u64;

    for line in reader.lines() {
        let line = line?;
        let entry: SessionEntry = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,    // skip malformed lines, don't abort
        };
        match entry.kind {
            SessionEntryKind::Meta { ref cwd, ref provider, ref model, .. } => {
                session_meta = Some(MetaCache {
                    folder: cwd.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                });
            }
            SessionEntryKind::Assistant { ref message } => {
                if let Some(meta) = &session_meta {
                    let usage = extract_usage(message);  // walks message.content
                    inserted += insert_message(
                        conn, path, &entry.id, meta, entry.timestamp, &usage,
                    )?;
                }
            }
            // … approval decisions later.
            _ => {}
        }
    }
    Ok(inserted)
}
```

`Usage` is already on the `Message`'s metadata; we read it as-is and
trust `cost_usd`. Stop-reason and TTFT aren't recorded today (TODO:
add to `SessionEntry` in a follow-up RFD; for v1 we leave them
nullable).

### Aggregator

```rust
// crates/pi-stats/src/aggregate.rs
#[derive(Debug, Serialize)]
pub struct DashboardStats {
    pub overall:     OverallStats,
    pub by_model:    Vec<ModelStats>,
    pub by_folder:   Vec<FolderStats>,
    pub time_series: Vec<TimeSeriesPoint>,
    pub approvals:   ApprovalStats,
}

pub fn dashboard(conn: &Connection) -> rusqlite::Result<DashboardStats> {
    Ok(DashboardStats {
        overall:     overall(conn)?,
        by_model:    by_model(conn)?,
        by_folder:   by_folder(conn)?,
        time_series: time_series(conn, 24 /* hours */)?,
        approvals:   approval_breakdown(conn)?,
    })
}

fn overall(c: &Connection) -> rusqlite::Result<OverallStats> {
    c.query_row(
        "SELECT
           COUNT(*), SUM(input_tokens), SUM(output_tokens),
           SUM(cache_read_tok), SUM(cost_usd),
           AVG(duration_ms), AVG(ttft_ms),
           SUM(CASE WHEN stop_reason='error' THEN 1 ELSE 0 END)
         FROM messages",
        [],
        |row| Ok(OverallStats { /* ... */ }),
    )
}
```

Buckets are: 24-h hourly (`timestamp_ms / 3600000`) and 90-d daily
(`timestamp_ms / 86400000`). Both materialised in SQL — no in-memory
aggregation, even at 100k rows.

### HTTP server

```rust
// crates/pi-stats/src/server.rs
pub fn router(state: AppState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/stats",            get(handlers::stats))
        .route("/api/stats/recent",     get(handlers::recent))
        .route("/api/stats/errors",     get(handlers::errors))
        .route("/api/stats/models",     get(handlers::by_model))
        .route("/api/stats/folders",    get(handlers::by_folder))
        .route("/api/stats/timeseries", get(handlers::timeseries))
        .route("/api/sync",             get(handlers::sync_now))
        .route("/api/request/{id}",     get(handlers::request_detail))
        .route("/healthz",              get(|| async { "ok" }))
        .nest_service("/", embedded_assets())
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}
```

Routes match oh-my-pi exactly — same JSON shapes — so the React UI
ports verbatim. `embedded_assets()` returns a tower service that
serves `assets/index.html` (and friends) from `include_bytes!`. Auth
is intentionally absent: bind to `127.0.0.1` only.

### CLI

`pi --stats <verb>` lands in `crates/pi-coding-agent/src/cli.rs` as a
new optional subcommand:

```rust
// cli.rs
#[arg(long = "stats", value_name = "VERB", num_args = 0..=1,
      default_missing_value = "server")]
pub stats: Option<String>,
// Allowed verbs: server | sync | json
#[arg(long = "stats-port", default_value_t = 3847)]
pub stats_port: u16,
```

Verb dispatch table:

| Verb     | Behaviour                                                   |
|----------|-------------------------------------------------------------|
| `server` | sync_all then bind axum on 127.0.0.1:`--stats-port`         |
| `sync`   | sync_all + print human summary, exit 0                      |
| `json`   | sync_all + print `DashboardStats` JSON to stdout, exit 0    |

The agent loop is short-circuited when `cli.stats.is_some()` —
`pi --stats` never starts a session.

## Test plan

1. **`tests/ingest_session_jsonl.rs`** — write a tempdir session JSONL
   with one Meta + one Assistant + one Usage; call `sync_all`; assert
   exactly one row in `messages` with the right cost/folder/model.
   Run again, assert idempotency (still 1 row).
2. **`tests/ingest_partial.rs`** — append more Assistant rows after
   the first sync; second sync only ingests the new rows
   (`byte_offset` advanced).
3. **`tests/aggregate_smoke.rs`** — seed three models × four sessions
   into an in-memory DB, assert `dashboard().by_model.len() == 3` and
   `overall.total_cost ≈ Σ`.
4. **`tests/server_routes.rs`** — `tower::ServiceExt::oneshot` against
   the router for each of the 9 routes, asserting status + JSON
   shape. No live socket needed.
5. **Manual smoke**: `pi --stats server` against the real
   `~/.pi/agent/sessions/`, hit `curl localhost:3847/api/stats`,
   eyeball numbers against `wc -l` on the JSONL files.

## Out of scope

- React/JS UI build pipeline. v1 ships a hand-rolled single-file HTML
  page that queries `/api/stats` directly. Bundling the oh-my-pi React
  app is a follow-up RFD (0011 — `pi-stats UI`).
- Approval-decision capture inside `auto_approve::AutoApproveGate`.
  The gate currently doesn't write `approval_decisions` lines into
  the session JSONL. Tracked separately (RFD 0012 — approval-decision
  trail). Until that lands, the `approvals` block is always zero.
- Multi-host / cloud sync. The DB is per-machine.
- Retention policies. v1 keeps all rows forever; manual `sqlite3
  stats.db 'DELETE FROM messages WHERE timestamp_ms < ?'` if needed.
- Streaming TTFT capture. Adding `Usage::ttft_ms` requires plumbing
  through every provider's stream loop; tracked as RFD 0013.

## Open questions

- **Embed JS or fall back to plaintext?** v1 plan is plaintext. If we
  embed React we hit the `include_bytes!` size budget for the static
  binary (~200 kB minified Preact + chart lib). Decide before MVP.
- **Should `pi --stats sync` be wired into a post-turn hook so the DB
  stays current without a separate command?** Likely yes (cheap, &lt;5
  ms per turn for incremental ingest), but punt to an issue.
- **One DB per cwd or one global DB?** Oh-my-pi uses one global. We
  match. Folder is a column, not a database. (Decided — listed for the
  record.)
