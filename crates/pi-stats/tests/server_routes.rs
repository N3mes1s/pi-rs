//! Test 4 from RFD 0004 §Test plan: hit each route via
//! `tower::ServiceExt::oneshot` (no live socket).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pi_stats::{open_in_memory, server};
use tower::ServiceExt;

fn state() -> server::AppState {
    let conn = open_in_memory().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    server::AppState::new(conn, tmp.path().to_path_buf())
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body)
}

#[tokio::test]
async fn all_nine_routes_respond() {
    let app = server::router(state());

    let routes = [
        "/api/stats",
        "/api/stats/recent",
        "/api/stats/errors",
        "/api/stats/models",
        "/api/stats/folders",
        "/api/stats/timeseries",
        "/api/sync",
        "/healthz",
    ];
    for uri in routes {
        let (status, body) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} not OK: {status}");
        assert!(!body.is_empty(), "{uri} body empty");
    }

    // /api/request/{id} for an unknown id should be 404.
    let (status, _) = get(app.clone(), "/api/request/999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Index page: text/html with the dashboard shell.
    let (status, body) = get(app.clone(), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("pi-stats"));
}

#[tokio::test]
async fn stats_payload_has_expected_shape() {
    let app = server::router(state());
    let (status, body) = get(app, "/api/stats").await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    for k in ["overall", "by_model", "by_folder", "time_series", "approvals"] {
        assert!(v.get(k).is_some(), "missing field: {k}");
    }
    let overall = &v["overall"];
    for k in [
        "total_requests",
        "total_input_tokens",
        "total_output_tokens",
        "total_cache_read_tokens",
        "total_cost",
        "avg_duration_ms",
        "avg_ttft_ms",
        "error_count",
    ] {
        assert!(overall.get(k).is_some(), "overall missing: {k}");
    }
}
