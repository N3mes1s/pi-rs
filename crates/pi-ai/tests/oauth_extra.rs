//! Extra coverage for the OAuth helpers: error paths in `exchange_code`,
//! the `refresh` flow, and the embedded callback listener.

use pi_ai::oauth::{
    build_authorize_url, exchange_code, listen_for_callback, refresh, OAuthEndpoints, Pkce,
};
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fake_endpoints(token_url: String) -> OAuthEndpoints {
    OAuthEndpoints {
        token_url,
        ..OAuthEndpoints::anthropic()
    }
}

#[tokio::test]
async fn exchange_code_returns_provider_error_on_non_2xx_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
        .mount(&server)
        .await;

    let ep = fake_endpoints(format!("{}/oauth/token", server.uri()));
    let pkce = Pkce::from_bytes(&[3u8; 32]);
    let client = reqwest::Client::new();
    let err = exchange_code(&client, &ep, &pkce, "the-code")
        .await
        .unwrap_err();
    let s = err.to_string();
    assert!(s.contains("401") || s.contains("provider"), "got: {s}");
}

#[tokio::test]
async fn exchange_code_propagates_unparseable_body_as_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        // 200 OK but body is not valid JSON for TokenResponse
        .respond_with(ResponseTemplate::new(200).set_body_string("garbage"))
        .mount(&server)
        .await;
    let ep = fake_endpoints(format!("{}/oauth/token", server.uri()));
    let pkce = Pkce::from_bytes(&[1u8; 32]);
    let client = reqwest::Client::new();
    let err = exchange_code(&client, &ep, &pkce, "code")
        .await
        .unwrap_err();
    let _ = err; // any AiError variant counts; we just want the path executed
}

#[tokio::test]
async fn refresh_returns_a_new_token_response_on_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&json!({
            "access_token": "new-access",
            "refresh_token": "rolling",
            "expires_in": 1800,
            "token_type": "Bearer",
        })))
        .mount(&server)
        .await;
    let ep = fake_endpoints(format!("{}/oauth/token", server.uri()));
    let client = reqwest::Client::new();
    let resp = refresh(&client, &ep, "old-refresh").await.unwrap();
    assert_eq!(resp.access_token, "new-access");
    assert_eq!(resp.refresh_token.as_deref(), Some("rolling"));
}

#[tokio::test]
async fn refresh_returns_provider_error_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
        .mount(&server)
        .await;
    let ep = fake_endpoints(format!("{}/oauth/token", server.uri()));
    let client = reqwest::Client::new();
    let r = refresh(&client, &ep, "stale").await;
    assert!(r.is_err());
}

#[tokio::test]
async fn listen_for_callback_captures_code_and_state_on_match() {
    // Bind to an ephemeral port and spawn the listener concurrently.
    let bind = "127.0.0.1:0".to_string();
    // We cannot directly inspect the chosen port via listen_for_callback —
    // use a separate listener to find a free port first, then close it.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let actual_bind = format!("127.0.0.1:{port}");
    let _ = bind;

    let listener = tokio::spawn({
        let b = actual_bind.clone();
        async move { listen_for_callback(&b, "EXPECTED_STATE").await }
    });

    // Give the listener a moment to bind.
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Send an HTTP request mimicking the OAuth provider redirect.
    let client = reqwest::Client::new();
    let url = format!(
        "http://{}/callback?code=THE_CODE&state=EXPECTED_STATE",
        actual_bind
    );
    // Fire-and-forget — the listener returns once it gets one connection.
    let _ = client
        .get(&url)
        .timeout(Duration::from_millis(500))
        .send()
        .await;

    let res = listener.await.expect("join");
    let (code, state) = res.expect("listen ok");
    assert_eq!(code, "THE_CODE");
    assert_eq!(state, "EXPECTED_STATE");
}

#[tokio::test]
async fn listen_for_callback_errors_on_state_mismatch() {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let actual_bind = format!("127.0.0.1:{port}");

    let listener = tokio::spawn({
        let b = actual_bind.clone();
        async move { listen_for_callback(&b, "EXPECTED_STATE").await }
    });
    tokio::time::sleep(Duration::from_millis(80)).await;

    let url = format!("http://{}/callback?code=X&state=WRONG_STATE", actual_bind);
    let client = reqwest::Client::new();
    let _ = client
        .get(&url)
        .timeout(Duration::from_millis(500))
        .send()
        .await;

    let r = listener.await.expect("join");
    assert!(r.is_err());
}

#[test]
fn build_authorize_url_state_round_trip_when_state_is_empty() {
    let ep = OAuthEndpoints::anthropic();
    let pkce = Pkce::from_bytes(&[5u8; 32]);
    let url = build_authorize_url(&ep, &pkce, "");
    assert!(url.contains("state="));
}
