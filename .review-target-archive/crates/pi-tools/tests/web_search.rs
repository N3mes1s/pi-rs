//! End-to-end coverage of the web_search tool against a wiremock
//! server. Verifies the request shape pi-rs sends to each provider
//! and that the per-provider response shapes are coerced into the
//! common SearchResult format.

use pi_tools::{
    web_search::{run_search, WebSearchConfig, WebSearchProvider, WebSearchTool},
    Tool, ToolContext,
};
use serde_json::json;
use std::time::Duration;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn cfg(server: &MockServer, provider: WebSearchProvider, key: &str) -> WebSearchConfig {
    WebSearchConfig {
        provider,
        api_key: key.into(),
        base_url: Some(server.uri()),
        max_results: 5,
        max_chars_per_result: 6000,
        timeout: Duration::from_secs(5),
    }
}

#[tokio::test]
async fn parallel_default_request_shape_and_response_parsing() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/alpha/search"))
        .and(header("x-api-key", "secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {
                    "url": "https://example.com/a",
                    "title": "First",
                    "excerpts": ["snippet one", "snippet two"]
                },
                {
                    "url": "https://example.com/b",
                    "title": "Second",
                    "content": "lorem ipsum"
                }
            ]
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let cfg = cfg(&server, WebSearchProvider::Parallel, "secret");
    let results = run_search(&http, &cfg, "rust async").await.unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].url, "https://example.com/a");
    assert_eq!(results[0].title, "First");
    assert!(results[0].snippet.contains("snippet one"));
    assert!(results[0].snippet.contains("snippet two"));
    assert_eq!(results[1].snippet, "lorem ipsum");

    // Verify the request body matches the documented shape.
    let req = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["objective"], "rust async");
    assert_eq!(body["search_queries"], json!(["rust async"]));
    assert_eq!(body["processor"], "base");
    assert_eq!(body["max_results"], 5);
    assert_eq!(body["max_chars_per_result"], 6000);
}

#[tokio::test]
async fn parallel_tool_invoke_round_trips_through_tool_contract() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/alpha/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"url": "https://x", "title": "X", "excerpts": ["one"]}
            ]
        })))
        .mount(&server)
        .await;

    let tool = WebSearchTool::with_config(cfg(&server, WebSearchProvider::Parallel, "k"));
    let res = tool
        .invoke(&ToolContext::default(), "id1", json!({"query": "q"}))
        .await
        .unwrap();
    assert!(!res.is_error);
    assert!(res.model_output.contains("[1] X"));
    assert!(res.model_output.contains("https://x"));
}

#[tokio::test]
async fn parallel_propagates_http_error_into_tool_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/alpha/search"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "boom"})))
        .mount(&server)
        .await;

    let tool = WebSearchTool::with_config(cfg(&server, WebSearchProvider::Parallel, "k"));
    let err = tool
        .invoke(&ToolContext::default(), "id1", json!({"query": "q"}))
        .await
        .expect_err("expected error");
    let msg = format!("{err:?}");
    assert!(msg.contains("HTTP 500"), "got {msg}");
}

#[tokio::test]
async fn exa_request_uses_x_api_key_and_search_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(header("x-api-key", "exa-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                {"url": "https://e", "title": "E", "text": "exa snippet"}
            ]
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let cfg = cfg(&server, WebSearchProvider::Exa, "exa-key");
    let r = run_search(&http, &cfg, "q").await.unwrap();
    assert_eq!(r[0].url, "https://e");
    assert_eq!(r[0].snippet, "exa snippet");
}

#[tokio::test]
async fn brave_uses_subscription_token_header_and_query_string() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(header("X-Subscription-Token", "brave-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": {
                "results": [
                    {"url": "https://b", "title": "B", "description": "brave hit"}
                ]
            }
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let cfg = cfg(&server, WebSearchProvider::Brave, "brave-key");
    let r = run_search(&http, &cfg, "q").await.unwrap();
    assert_eq!(r[0].url, "https://b");
    assert_eq!(r[0].snippet, "brave hit");
}

#[tokio::test]
async fn jina_uses_bearer_token_and_data_array() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("Authorization", "Bearer jk"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"url": "https://j", "title": "J", "content": "jina"}
            ]
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let cfg = cfg(&server, WebSearchProvider::Jina, "jk");
    let r = run_search(&http, &cfg, "q").await.unwrap();
    assert_eq!(r[0].url, "https://j");
}

#[tokio::test]
async fn perplexity_extracts_citations_and_summary() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "summary text"}}],
            "citations": ["https://p1", "https://p2"]
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let cfg = cfg(&server, WebSearchProvider::Perplexity, "pp");
    let r = run_search(&http, &cfg, "q").await.unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].url, "https://p1");
    assert_eq!(r[1].url, "https://p2");
    assert_eq!(r[0].snippet, "summary text");
}
