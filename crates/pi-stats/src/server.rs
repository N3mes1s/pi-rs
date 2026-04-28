//! HTTP server. Mirrors the oh-my-pi `omp-stats` route shapes.

use crate::{aggregate, ingest};
use axum::{
    extract::{Path as AxPath, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
    pub sessions_root: PathBuf,
}

impl AppState {
    pub fn new(conn: Connection, sessions_root: PathBuf) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            sessions_root,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/stats", get(stats))
        .route("/api/stats/recent", get(recent))
        .route("/api/stats/errors", get(errors))
        .route("/api/stats/models", get(by_model))
        .route("/api/stats/folders", get(by_folder))
        .route("/api/stats/timeseries", get(timeseries))
        .route("/api/sync", get(sync_now))
        .route("/api/request/:id", get(request_detail))
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(index))
        .route("/index.html", get(index))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state)
}

const INDEX_HTML: &str = include_str!("../assets/index.html");

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

fn err(e: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response()
}

async fn stats(State(s): State<AppState>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::dashboard(&conn) {
        Ok(d) => Json(d).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct LimitQ {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    50
}

async fn recent(State(s): State<AppState>, Query(q): Query<LimitQ>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::recent(&conn, q.limit) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn errors(State(s): State<AppState>, Query(q): Query<LimitQ>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::errors(&conn, q.limit) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn by_model(State(s): State<AppState>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::by_model(&conn) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn by_folder(State(s): State<AppState>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::by_folder(&conn) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct TimeseriesQ {
    #[serde(default = "default_hours")]
    hours: i64,
}

fn default_hours() -> i64 {
    24
}

async fn timeseries(State(s): State<AppState>, Query(q): Query<TimeseriesQ>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::time_series_hourly(&conn, q.hours) {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn sync_now(State(s): State<AppState>) -> Response {
    let mut conn = s.conn.lock().unwrap();
    match ingest::sync_all(&mut conn, &s.sessions_root) {
        Ok(report) => Json(json!({
            "files": report.files,
            "rows": report.rows,
        }))
        .into_response(),
        Err(e) => err(e),
    }
}

async fn request_detail(State(s): State<AppState>, AxPath(id): AxPath<i64>) -> Response {
    let conn = s.conn.lock().unwrap();
    match aggregate::request_detail(&conn, id) {
        Ok(Some(r)) => Json(r).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(Value::Null)).into_response(),
        Err(e) => err(e),
    }
}

/// Bind to `127.0.0.1:port` and serve until the process is killed.
pub async fn serve(state: AppState, port: u16) -> anyhow::Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!(%addr, "pi-stats: listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await?;
    Ok(())
}
