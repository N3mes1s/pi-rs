//! SQL → `DashboardStats`. Buckets are computed in SQL: 24-hour
//! hourly time-series, plus model/folder breakdowns.

use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct OverallStats {
    pub total_requests: u64,
    pub total_sessions: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cost: f64,
    pub avg_duration_ms: f64,
    pub avg_ttft_ms: f64,
    pub error_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStats {
    pub model: String,
    pub provider: String,
    pub requests: u64,
    pub sessions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderStats {
    pub folder: String,
    pub requests: u64,
    pub cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimeSeriesPoint {
    pub bucket_ms: i64,
    pub requests: u64,
    pub cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApprovalStats {
    pub total: u64,
    pub allow: u64,
    pub ask: u64,
    pub deny: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardStats {
    pub overall: OverallStats,
    pub by_model: Vec<ModelStats>,
    pub by_folder: Vec<FolderStats>,
    pub time_series: Vec<TimeSeriesPoint>,
    pub approvals: ApprovalStats,
    pub by_route_id: Vec<RouteStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteStats {
    pub route_id: String,
    pub decisions: u64,
    pub sessions: u64,
    pub avg_budget_tokens: Option<f64>,
}

pub fn dashboard(conn: &Connection) -> rusqlite::Result<DashboardStats> {
    Ok(DashboardStats {
        overall: overall(conn)?,
        by_model: by_model(conn)?,
        by_folder: by_folder(conn)?,
        time_series: time_series_hourly(conn, 24)?,
        approvals: approval_breakdown(conn)?,
        by_route_id: by_route_id(conn)?,
    })
}

pub fn by_route_id(c: &Connection) -> rusqlite::Result<Vec<RouteStats>> {
    let mut stmt = c.prepare(
        "SELECT route_id,
                COUNT(*),
                COUNT(DISTINCT session_file),
                AVG(budget_tokens)
           FROM routing_decisions
          GROUP BY route_id
          ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(RouteStats {
            route_id: r.get(0)?,
            decisions: r.get::<_, i64>(1)? as u64,
            sessions: r.get::<_, i64>(2)? as u64,
            avg_budget_tokens: r.get::<_, Option<f64>>(3)?,
        })
    })?;
    rows.collect()
}

pub fn overall(c: &Connection) -> rusqlite::Result<OverallStats> {
    c.query_row(
        "SELECT
           COUNT(*),
           COUNT(DISTINCT session_file),
           COALESCE(SUM(input_tokens), 0),
           COALESCE(SUM(output_tokens), 0),
           COALESCE(SUM(cache_read_tok), 0),
           COALESCE(SUM(cost_usd), 0.0),
           COALESCE(AVG(duration_ms), 0.0),
           COALESCE(AVG(ttft_ms), 0.0),
           COALESCE(SUM(CASE WHEN stop_reason='error' THEN 1 ELSE 0 END), 0)
         FROM messages",
        [],
        |row| {
            Ok(OverallStats {
                total_requests: row.get::<_, i64>(0)? as u64,
                total_sessions: row.get::<_, i64>(1)? as u64,
                total_input_tokens: row.get::<_, i64>(2)? as u64,
                total_output_tokens: row.get::<_, i64>(3)? as u64,
                total_cache_read_tokens: row.get::<_, i64>(4)? as u64,
                total_cost: row.get(5)?,
                avg_duration_ms: row.get(6)?,
                avg_ttft_ms: row.get(7)?,
                error_count: row.get::<_, i64>(8)? as u64,
            })
        },
    )
}

pub fn by_model(c: &Connection) -> rusqlite::Result<Vec<ModelStats>> {
    let mut stmt = c.prepare(
        "SELECT model, provider, COUNT(*),
                COUNT(DISTINCT session_file),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cache_read_tok), 0),
                COALESCE(SUM(cost_usd), 0.0)
           FROM messages
          GROUP BY model, provider
          ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ModelStats {
            model: r.get(0)?,
            provider: r.get(1)?,
            requests: r.get::<_, i64>(2)? as u64,
            sessions: r.get::<_, i64>(3)? as u64,
            input_tokens: r.get::<_, i64>(4)? as u64,
            output_tokens: r.get::<_, i64>(5)? as u64,
            cache_read_tokens: r.get::<_, i64>(6)? as u64,
            cost: r.get(7)?,
        })
    })?;
    rows.collect()
}

pub fn by_folder(c: &Connection) -> rusqlite::Result<Vec<FolderStats>> {
    let mut stmt = c.prepare(
        "SELECT folder, COUNT(*),
                COALESCE(SUM(cost_usd), 0.0),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0)
           FROM messages
          GROUP BY folder
          ORDER BY SUM(cost_usd) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(FolderStats {
            folder: r.get(0)?,
            requests: r.get::<_, i64>(1)? as u64,
            cost: r.get(2)?,
            input_tokens: r.get::<_, i64>(3)? as u64,
            output_tokens: r.get::<_, i64>(4)? as u64,
        })
    })?;
    rows.collect()
}

/// Hourly buckets for the last `hours` hours.
pub fn time_series_hourly(c: &Connection, hours: i64) -> rusqlite::Result<Vec<TimeSeriesPoint>> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let cutoff = now_ms - hours * 3_600_000;
    let mut stmt = c.prepare(
        "SELECT (timestamp_ms / 3600000) * 3600000 AS bucket,
                COUNT(*),
                COALESCE(SUM(cost_usd), 0.0),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0)
           FROM messages
          WHERE timestamp_ms >= ?1
          GROUP BY bucket
          ORDER BY bucket ASC",
    )?;
    let rows = stmt.query_map(params![cutoff], |r| {
        Ok(TimeSeriesPoint {
            bucket_ms: r.get(0)?,
            requests: r.get::<_, i64>(1)? as u64,
            cost: r.get(2)?,
            input_tokens: r.get::<_, i64>(3)? as u64,
            output_tokens: r.get::<_, i64>(4)? as u64,
        })
    })?;
    rows.collect()
}

pub fn approval_breakdown(c: &Connection) -> rusqlite::Result<ApprovalStats> {
    c.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN decision='allow' THEN 1 ELSE 0 END),
                SUM(CASE WHEN decision='ask'   THEN 1 ELSE 0 END),
                SUM(CASE WHEN decision='deny'  THEN 1 ELSE 0 END)
           FROM approval_decisions",
        [],
        |r| {
            Ok(ApprovalStats {
                total: r.get::<_, i64>(0)? as u64,
                allow: r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                ask: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                deny: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
            })
        },
    )
}

/// One row per assistant request, recent-first. Used by `/api/stats/recent`.
#[derive(Debug, Clone, Serialize)]
pub struct RecentRow {
    pub id: i64,
    pub timestamp_ms: i64,
    pub folder: String,
    pub model: String,
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    pub stop_reason: String,
}

pub fn recent(c: &Connection, limit: i64) -> rusqlite::Result<Vec<RecentRow>> {
    let mut stmt = c.prepare(
        "SELECT id, timestamp_ms, folder, model, provider,
                input_tokens, output_tokens, cost_usd, stop_reason
           FROM messages
          ORDER BY timestamp_ms DESC
          LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(RecentRow {
            id: r.get(0)?,
            timestamp_ms: r.get(1)?,
            folder: r.get(2)?,
            model: r.get(3)?,
            provider: r.get(4)?,
            input_tokens: r.get::<_, i64>(5)? as u64,
            output_tokens: r.get::<_, i64>(6)? as u64,
            cost: r.get(7)?,
            stop_reason: r.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn errors(c: &Connection, limit: i64) -> rusqlite::Result<Vec<RecentRow>> {
    let mut stmt = c.prepare(
        "SELECT id, timestamp_ms, folder, model, provider,
                input_tokens, output_tokens, cost_usd, stop_reason
           FROM messages
          WHERE stop_reason='error'
          ORDER BY timestamp_ms DESC
          LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(RecentRow {
            id: r.get(0)?,
            timestamp_ms: r.get(1)?,
            folder: r.get(2)?,
            model: r.get(3)?,
            provider: r.get(4)?,
            input_tokens: r.get::<_, i64>(5)? as u64,
            output_tokens: r.get::<_, i64>(6)? as u64,
            cost: r.get(7)?,
            stop_reason: r.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn request_detail(c: &Connection, id: i64) -> rusqlite::Result<Option<RecentRow>> {
    let mut stmt = c.prepare(
        "SELECT id, timestamp_ms, folder, model, provider,
                input_tokens, output_tokens, cost_usd, stop_reason
           FROM messages WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], |r| {
        Ok(RecentRow {
            id: r.get(0)?,
            timestamp_ms: r.get(1)?,
            folder: r.get(2)?,
            model: r.get(3)?,
            provider: r.get(4)?,
            input_tokens: r.get::<_, i64>(5)? as u64,
            output_tokens: r.get::<_, i64>(6)? as u64,
            cost: r.get(7)?,
            stop_reason: r.get(8)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}
