//! Smoke tests for `pi_sandbox::cache::RootfsCache`.
//!
//! Uses `wiremock` for HTTP mocking and `tempfile` for isolated cache
//! directories. Each test gets its own temp dir so they don't interfere.
//!
//! Tests that touch process-wide environment variables (PI_SANDBOX_ROOTFS,
//! PI_SANDBOX_OFFLINE) acquire a shared mutex so they don't race each other.

use pi_sandbox::cache::{CacheError, RootfsCache};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use tempfile::TempDir;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Global mutex for tests that mutate env vars. Using `unwrap_or_else` to
/// handle a poisoned lock if a prior test panicked while holding it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the env lock; recover from a poisoned state (prior test panicked).
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Compute sha256 of a byte slice (for test fixtures).
fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Build a `RootfsCache` rooted at the given temp dir.
fn cache(tmp: &TempDir) -> RootfsCache {
    RootfsCache::new(tmp.path().to_path_buf())
}

/// Clear both sandbox env-var overrides.
fn clear_env() {
    std::env::remove_var("PI_SANDBOX_ROOTFS");
    std::env::remove_var("PI_SANDBOX_OFFLINE");
}

// ---------------------------------------------------------------------------
// Happy path: file absent → download → sha matches → returns path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_download_and_verify() {
    let body = b"fake rootfs content for testing";
    let expected_sha = sha256_hex(body);
    let expected_size = body.len() as u64;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rootfs.img.zst"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let c = cache(&tmp);
    let url = format!("{}/rootfs.img.zst", server.uri());

    let _guard = lock_env();
    clear_env();

    let result = c
        .ensure("0.1.0", &url, &expected_sha, expected_size)
        .await
        .unwrap();

    assert!(result.exists(), "artifact should exist on disk");
    let on_disk = std::fs::read(&result).unwrap();
    assert_eq!(on_disk, body);
}

// ---------------------------------------------------------------------------
// Already cached + valid sha → no HTTP request issued
// ---------------------------------------------------------------------------

#[tokio::test]
async fn already_cached_valid_sha_no_request() {
    let body = b"cached content";
    let expected_sha = sha256_hex(body);
    let expected_size = body.len() as u64;

    let server = MockServer::start().await;
    // No mock mounted — any request would be unmatched.

    let tmp = tempfile::tempdir().unwrap();
    let c = cache(&tmp);

    // Pre-populate the cache.
    let artifact_path = c.artifact_path("0.1.0");
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(&artifact_path, body).unwrap();

    let url = format!("{}/rootfs.img.zst", server.uri());

    let _guard = lock_env();
    clear_env();

    let result = c
        .ensure("0.1.0", &url, &expected_sha, expected_size)
        .await
        .unwrap();

    assert_eq!(result, artifact_path);

    // Assert the wiremock server received zero requests.
    let reqs = server.received_requests().await.unwrap_or_default();
    assert_eq!(reqs.len(), 0, "should not have issued any HTTP request");
}

// ---------------------------------------------------------------------------
// Already cached + bad sha → delete + re-download
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cached_bad_sha_triggers_redownload() {
    let fresh_body = b"fresh rootfs data after redownload.";
    // The "correct" sha is of the fresh content.
    let correct_sha = sha256_hex(fresh_body);
    // Pre-populate with same-length but wrong content (complete file, sha mismatch).
    let stale_same_len: Vec<u8> = vec![0xAA; fresh_body.len()];
    let expected_size = fresh_body.len() as u64;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rootfs.img.zst"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(fresh_body.to_vec()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let c = cache(&tmp);

    // Pre-populate with stale content of same length (full-size but wrong sha).
    let artifact_path = c.artifact_path("0.1.0");
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(&artifact_path, &stale_same_len).unwrap();

    let url = format!("{}/rootfs.img.zst", server.uri());

    let _guard = lock_env();
    clear_env();

    let result = c
        .ensure("0.1.0", &url, &correct_sha, expected_size)
        .await
        .unwrap();

    let on_disk = std::fs::read(&result).unwrap();
    assert_eq!(on_disk, fresh_body, "should have replaced stale content");

    let reqs = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        reqs.len(),
        1,
        "should have issued exactly one download request"
    );
}

// ---------------------------------------------------------------------------
// 4xx response → CacheError::Http
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_4xx_returns_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rootfs.img.zst"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let c = cache(&tmp);
    let url = format!("{}/rootfs.img.zst", server.uri());

    let _guard = lock_env();
    clear_env();

    let err = c.ensure("0.1.0", &url, "aaaa", 100).await.unwrap_err();

    match err {
        CacheError::Http(msg) => {
            assert!(msg.contains("404"), "expected 404 in error: {msg}");
        }
        other => panic!("expected Http error, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// PI_SANDBOX_OFFLINE=1 + missing artifact → CacheError::OfflineMissingRootfs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn offline_mode_missing_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cache(&tmp);

    let _guard = lock_env();
    clear_env();
    std::env::set_var("PI_SANDBOX_OFFLINE", "1");

    let err = c
        .ensure(
            "0.1.0",
            "http://127.0.0.1:1/rootfs.img.zst",
            "aaaa",
            100,
        )
        .await
        .unwrap_err();

    clear_env();

    match err {
        CacheError::OfflineMissingRootfs => {}
        other => panic!("expected OfflineMissingRootfs, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// PI_SANDBOX_ROOTFS env override → return that path without HTTP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn env_override_skips_download() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().join("my_custom_rootfs.img.zst");
    std::fs::write(&override_path, b"custom rootfs").unwrap();

    let server = MockServer::start().await;
    // No mock — any HTTP request would cause an unmatched-request panic.

    let _guard = lock_env();
    clear_env();
    std::env::set_var("PI_SANDBOX_ROOTFS", override_path.to_str().unwrap());

    let c = cache(&tmp);
    let result = c
        .ensure("0.1.0", &format!("{}/x", server.uri()), "any-sha", 0)
        .await
        .unwrap();

    clear_env();

    assert_eq!(result, override_path);

    let reqs = server.received_requests().await.unwrap_or_default();
    assert_eq!(reqs.len(), 0, "should not contact server with env override");
}

// ---------------------------------------------------------------------------
// Resume: partial file → request includes Range header
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_download_sends_range_header() {
    // Pre-write the first half, then serve the second half.
    let full_body: Vec<u8> = (0u8..=127).collect(); // 128 bytes
    let half = full_body.len() / 2; // 64
    let second_half = full_body[half..].to_vec();
    let expected_sha = sha256_hex(&full_body);
    let expected_size = full_body.len() as u64;

    let server = MockServer::start().await;
    // Respond with the second half (206 Partial Content) for the Range request.
    Mock::given(method("GET"))
        .and(path("/rootfs.img.zst"))
        .respond_with(
            ResponseTemplate::new(206).set_body_bytes(second_half.clone()),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let c = cache(&tmp);

    // Pre-populate with first half (simulates interrupted download).
    // File size < expected_size → treated as partial by ensure().
    let artifact_path = c.artifact_path("0.1.0");
    std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
    std::fs::write(&artifact_path, &full_body[..half]).unwrap();

    let url = format!("{}/rootfs.img.zst", server.uri());

    let _guard = lock_env();
    clear_env();

    let result = c
        .ensure("0.1.0", &url, &expected_sha, expected_size)
        .await
        .unwrap();

    let on_disk = std::fs::read(&result).unwrap();
    assert_eq!(
        on_disk, full_body,
        "resumed download should produce complete file"
    );

    let reqs = server.received_requests().await.unwrap_or_default();
    assert_eq!(reqs.len(), 1, "exactly one range request");
    // Verify the Range header was sent with the correct offset.
    let range_hdr = reqs[0]
        .headers
        .get("range")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert_eq!(
        range_hdr,
        format!("bytes={half}-"),
        "Range header should request remaining bytes"
    );
}
