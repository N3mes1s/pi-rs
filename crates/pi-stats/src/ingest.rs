//! JSONL → SQLite ingest. Walks `<sessions_root>/<cwd-slug>/*.jsonl`
//! and persists one row per `Assistant` entry, attaching the next
//! following `Usage` line for token/cost data. Idempotent via the
//! `file_offsets` watermark table (byte offset + mtime).

use crate::schema;
use pi_agent_core::session::{SessionEntry, SessionEntryKind};
use rusqlite::{params, Connection};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct SyncReport {
    pub files: u64,
    pub rows: u64,
}

#[derive(Debug, Clone)]
struct MetaCache {
    folder: String,
    provider: String,
    model: String,
}

/// Walk `sessions_root` recursively and ingest every `*.jsonl` file.
/// Idempotent: a second call with no new bytes inserts zero rows.
pub fn sync_all(conn: &mut Connection, sessions_root: &Path) -> anyhow::Result<SyncReport> {
    schema::ensure(conn)?;
    let mut report = SyncReport::default();
    if !sessions_root.exists() {
        return Ok(report);
    }
    for entry in walkdir::WalkDir::new(sessions_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file() && e.path().extension().map_or(false, |ext| ext == "jsonl")
        })
    {
        let path = entry.path();
        let meta = std::fs::metadata(path)?;
        let mtime_ms = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let file_len = meta.len();
        let (prev_offset, prev_mtime) = read_offset(conn, path)?;
        // Skip cleanly when the file hasn't grown and mtime is unchanged.
        // Both conditions are required: mtime alone can lie when files are
        // rewritten in place; size alone misses same-size edits.
        if prev_offset == file_len && prev_mtime == mtime_ms && mtime_ms != 0 {
            report.files += 1;
            continue;
        }
        // Always re-walk the whole file; UNIQUE(session_file, entry_id)
        // makes it idempotent. The watermark is purely an "is anything
        // new?" gate, so cross-run pairing of Assistant+Usage Just Works.
        let inserted = ingest_one(conn, path)?;
        write_offset(conn, path, file_len, mtime_ms)?;
        report.files += 1;
        report.rows += inserted;
    }
    Ok(report)
}

fn ingest_one(conn: &Connection, path: &Path) -> anyhow::Result<u64> {
    let path_key = path_key(path);
    let f = std::fs::File::open(path)?;
    let reader = BufReader::new(f);

    // Track session metadata as we go; the Meta line lives at the head
    // but in principle could be re-emitted. Track the most recent
    // assistant entry id so the next-following Usage line attaches to it.
    let mut session_meta: Option<MetaCache> = None;
    let mut last_assistant: Option<String> = None;
    let mut inserted = 0u64;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionEntry = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match &entry.kind {
            SessionEntryKind::Meta {
                cwd,
                provider,
                model,
                ..
            } => {
                session_meta = Some(MetaCache {
                    folder: cwd.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                });
            }
            SessionEntryKind::Assistant { .. } => {
                if let Some(meta) = &session_meta {
                    let n =
                        insert_message(conn, &path_key, &entry.id, meta, entry.timestamp, None)?;
                    inserted += n;
                    last_assistant = Some(entry.id.clone());
                }
            }
            SessionEntryKind::Usage { usage } => {
                if let Some(asst_id) = &last_assistant {
                    update_message_usage(conn, &path_key, asst_id, usage)?;
                    last_assistant = None;
                }
            }
            _ => {}
        }
    }
    Ok(inserted)
}

fn insert_message(
    conn: &Connection,
    session_file: &str,
    entry_id: &str,
    meta: &MetaCache,
    timestamp_ms: i64,
    usage: Option<&pi_ai::Usage>,
) -> rusqlite::Result<u64> {
    let (input, output, cache_read, cache_write, reasoning, cost) = match usage {
        Some(u) => (
            u.input_tokens as i64,
            u.output_tokens as i64,
            u.cache_read_tokens as i64,
            u.cache_write_tokens as i64,
            u.reasoning_tokens as i64,
            u.cost_usd,
        ),
        None => (0, 0, 0, 0, 0, 0.0),
    };
    let total = input + output + cache_read + cache_write + reasoning;
    let n = conn.execute(
        "INSERT OR IGNORE INTO messages (
            session_file, entry_id, folder, model, provider,
            timestamp_ms, duration_ms, ttft_ms, stop_reason, error_message,
            input_tokens, output_tokens, cache_read_tok, cache_write_tok,
            reasoning_tok, total_tokens, cost_usd
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, NULL, NULL, ?7, NULL,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14
        )",
        params![
            session_file,
            entry_id,
            meta.folder,
            meta.model,
            meta.provider,
            timestamp_ms,
            "stop",
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
            total,
            cost,
        ],
    )?;
    Ok(n as u64)
}

fn update_message_usage(
    conn: &Connection,
    session_file: &str,
    entry_id: &str,
    usage: &pi_ai::Usage,
) -> rusqlite::Result<()> {
    let input = usage.input_tokens as i64;
    let output = usage.output_tokens as i64;
    let cache_read = usage.cache_read_tokens as i64;
    let cache_write = usage.cache_write_tokens as i64;
    let reasoning = usage.reasoning_tokens as i64;
    let total = input + output + cache_read + cache_write + reasoning;
    conn.execute(
        "UPDATE messages
            SET input_tokens=?1, output_tokens=?2, cache_read_tok=?3,
                cache_write_tok=?4, reasoning_tok=?5, total_tokens=?6,
                cost_usd=?7
          WHERE session_file=?8 AND entry_id=?9",
        params![
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
            total,
            usage.cost_usd,
            session_file,
            entry_id,
        ],
    )?;
    Ok(())
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_offset(conn: &Connection, path: &Path) -> rusqlite::Result<(u64, i64)> {
    let key = path_key(path);
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT byte_offset, last_modified_ms FROM file_offsets WHERE session_file=?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    Ok(match row {
        Some((off, mt)) => (off.max(0) as u64, mt),
        None => (0, 0),
    })
}

fn write_offset(
    conn: &Connection,
    path: &Path,
    byte_offset: u64,
    mtime_ms: i64,
) -> rusqlite::Result<()> {
    let key = path_key(path);
    conn.execute(
        "INSERT INTO file_offsets (session_file, byte_offset, last_modified_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(session_file) DO UPDATE SET
            byte_offset=excluded.byte_offset,
            last_modified_ms=excluded.last_modified_ms",
        params![key, byte_offset as i64, mtime_ms],
    )?;
    Ok(())
}

#[allow(dead_code)]
fn reset_offset(conn: &Connection, path: &Path) -> rusqlite::Result<()> {
    let key = path_key(path);
    conn.execute(
        "DELETE FROM file_offsets WHERE session_file=?1",
        params![key],
    )?;
    Ok(())
}

/// Default sessions root: `<agent_dir>/sessions`.
pub fn default_sessions_root() -> PathBuf {
    if let Ok(p) = std::env::var("PI_CODING_AGENT_DIR") {
        return PathBuf::from(p).join("sessions");
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("agent").join("sessions")
}

/// Default DB path: `<agent_dir>/stats.db`.
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("PI_CODING_AGENT_DIR") {
        return PathBuf::from(p).join("stats.db");
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("agent").join("stats.db")
}
