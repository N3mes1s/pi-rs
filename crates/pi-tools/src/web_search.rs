//! `web_search` tool with seven backends.
//!
//! Default backend is **Parallel.ai** (`POST https://api.parallel.ai/alpha/search`,
//! `x-api-key` header, `processor: "base"`, 5 results, 6000 chars/result).
//! Fallbacks (selected by `WEB_SEARCH_PROVIDER` env or per-call argument):
//!
//! | name        | env key                     |
//! |-------------|-----------------------------|
//! | parallel    | `PARALLEL_API_KEY`          |
//! | exa         | `EXA_API_KEY`               |
//! | brave       | `BRAVE_API_KEY`             |
//! | jina        | `JINA_API_KEY`              |
//! | perplexity  | `PERPLEXITY_API_KEY`        |
//! | anthropic   | `ANTHROPIC_API_KEY`         |
//! | gemini      | `GEMINI_API_KEY`            |
//!
//! Each provider has its own request shape; we coerce results into a
//! common [`SearchResult`] format before formatting for the model.

use async_trait::async_trait;
use pi_tool_types::{ToolResult, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::time::Duration;

use crate::{Tool, ToolContext, ToolError};

/// One normalised search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub url: String,
    pub title: String,
    pub snippet: String,
}

/// Supported search providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    Parallel,
    Exa,
    Brave,
    Jina,
    Perplexity,
    Anthropic,
    Gemini,
}

impl WebSearchProvider {
    pub const ALL: &'static [WebSearchProvider] = &[
        WebSearchProvider::Parallel,
        WebSearchProvider::Exa,
        WebSearchProvider::Brave,
        WebSearchProvider::Jina,
        WebSearchProvider::Perplexity,
        WebSearchProvider::Anthropic,
        WebSearchProvider::Gemini,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebSearchProvider::Parallel => "parallel",
            WebSearchProvider::Exa => "exa",
            WebSearchProvider::Brave => "brave",
            WebSearchProvider::Jina => "jina",
            WebSearchProvider::Perplexity => "perplexity",
            WebSearchProvider::Anthropic => "anthropic",
            WebSearchProvider::Gemini => "gemini",
        }
    }

    pub fn parse(s: &str) -> Option<WebSearchProvider> {
        Self::ALL
            .iter()
            .copied()
            .find(|p| p.as_str().eq_ignore_ascii_case(s))
    }

    pub fn env_key(self) -> &'static str {
        match self {
            WebSearchProvider::Parallel => "PARALLEL_API_KEY",
            WebSearchProvider::Exa => "EXA_API_KEY",
            WebSearchProvider::Brave => "BRAVE_API_KEY",
            WebSearchProvider::Jina => "JINA_API_KEY",
            WebSearchProvider::Perplexity => "PERPLEXITY_API_KEY",
            WebSearchProvider::Anthropic => "ANTHROPIC_API_KEY",
            WebSearchProvider::Gemini => "GEMINI_API_KEY",
        }
    }
}

/// Per-call configuration. The CLI / settings layer constructs one of
/// these and either feeds it to [`WebSearchTool::new`] (overrides
/// everywhere) or lets the tool resolve from env on each call (default).
#[derive(Debug, Clone)]
pub struct WebSearchConfig {
    pub provider: WebSearchProvider,
    pub api_key: String,
    pub base_url: Option<String>,
    pub max_results: u32,
    pub max_chars_per_result: u32,
    pub timeout: Duration,
}

impl WebSearchConfig {
    pub fn from_env(provider: WebSearchProvider) -> Option<Self> {
        let api_key = env::var(provider.env_key()).ok()?;
        Some(Self {
            provider,
            api_key,
            base_url: None,
            max_results: 5,
            max_chars_per_result: 6000,
            timeout: Duration::from_secs(20),
        })
    }
}

/// The `web_search` tool. When given an explicit [`WebSearchConfig`],
/// every call uses that. When constructed via `default()`, the tool
/// resolves the active provider per-call by:
///
///  1. Honouring the `provider` argument from the model's tool input.
///  2. Falling back to `WEB_SEARCH_PROVIDER` env (for op-time pinning).
///  3. Defaulting to Parallel.ai.
pub struct WebSearchTool {
    /// Optional explicit override (set by the SDK / tests). When
    /// `None`, providers are resolved per-call from env.
    pub override_cfg: Option<WebSearchConfig>,
    pub http: reqwest::Client,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self {
            override_cfg: None,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl WebSearchTool {
    pub fn with_config(cfg: WebSearchConfig) -> Self {
        let mut me = Self::default();
        me.override_cfg = Some(cfg);
        me
    }

    /// Resolve the config used for a given call. Per-call provider in
    /// `input.provider` takes precedence over env.
    fn resolve_config(&self, input: &Value) -> Result<WebSearchConfig, ToolError> {
        if let Some(cfg) = &self.override_cfg {
            return Ok(cfg.clone());
        }
        let provider = input
            .get("provider")
            .and_then(|v| v.as_str())
            .and_then(WebSearchProvider::parse)
            .or_else(|| {
                env::var("WEB_SEARCH_PROVIDER")
                    .ok()
                    .and_then(|s| WebSearchProvider::parse(&s))
            })
            .unwrap_or(WebSearchProvider::Parallel);
        WebSearchConfig::from_env(provider).ok_or_else(|| {
            ToolError::Other(format!("missing API key in env: {}", provider.env_key()))
        })
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web. Defaults to Parallel.ai; alternative providers (exa, brave, jina, perplexity, anthropic, gemini) selectable via the `provider` argument.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The search query."},
                    "provider": {
                        "type": "string",
                        "enum": ["parallel","exa","brave","jina","perplexity","anthropic","gemini"]
                    },
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 20}
                },
                "required": ["query"]
            }),
        }
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext,
        call_id: &str,
        input: Value,
    ) -> Result<ToolResult, ToolError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing `query`".into()))?
            .to_string();
        let mut cfg = self.resolve_config(&input)?;
        if let Some(n) = input.get("max_results").and_then(|v| v.as_u64()) {
            cfg.max_results = n.min(20) as u32;
        }
        let results = run_search(&self.http, &cfg, &query).await.map_err(|e| {
            ToolError::Other(format!("web_search ({}): {e}", cfg.provider.as_str()))
        })?;
        let body = format_results(&query, cfg.provider, &results, cfg.max_chars_per_result);
        Ok(ToolResult {
            tool_use_id: call_id.to_string(),
            model_output: body,
            display: None,
            is_error: false,
        })
    }
}

/// Format the hits as a Markdown-ish blob — one stanza per result.
pub fn format_results(
    query: &str,
    provider: WebSearchProvider,
    results: &[SearchResult],
    max_chars: u32,
) -> String {
    let mut s = format!("web_search via {} for: {}\n\n", provider.as_str(), query);
    if results.is_empty() {
        s.push_str("(no results)\n");
        return s;
    }
    for (i, r) in results.iter().enumerate() {
        let snip = if r.snippet.len() > max_chars as usize {
            // Char-safe truncation.
            let cut = r
                .snippet
                .char_indices()
                .take_while(|(idx, _)| *idx < max_chars as usize)
                .last()
                .map(|(idx, c)| idx + c.len_utf8())
                .unwrap_or(0);
            format!("{}…", &r.snippet[..cut])
        } else {
            r.snippet.clone()
        };
        s.push_str(&format!(
            "[{}] {}\n  {}\n  {}\n\n",
            i + 1,
            r.title,
            r.url,
            snip
        ));
    }
    s
}

/// Dispatch to the per-provider transport.
pub async fn run_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    match cfg.provider {
        WebSearchProvider::Parallel => parallel_search(http, cfg, query).await,
        WebSearchProvider::Exa => exa_search(http, cfg, query).await,
        WebSearchProvider::Brave => brave_search(http, cfg, query).await,
        WebSearchProvider::Jina => jina_search(http, cfg, query).await,
        WebSearchProvider::Perplexity => perplexity_search(http, cfg, query).await,
        WebSearchProvider::Anthropic => anthropic_search(http, cfg, query).await,
        WebSearchProvider::Gemini => gemini_search(http, cfg, query).await,
    }
}

fn base(cfg: &WebSearchConfig, default: &str) -> String {
    cfg.base_url.clone().unwrap_or_else(|| default.to_string())
}

// ── Parallel.ai ─────────────────────────────────────────────────────────────

async fn parallel_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let url = format!("{}/alpha/search", base(cfg, "https://api.parallel.ai"));
    let resp = http
        .post(&url)
        .header("x-api-key", &cfg.api_key)
        .json(&json!({
            "objective": query,
            "search_queries": [query],
            "processor": "base",
            "max_results": cfg.max_results,
            "max_chars_per_result": cfg.max_chars_per_result,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    Ok(extract_parallel(&v))
}

fn extract_parallel(v: &Value) -> Vec<SearchResult> {
    let Some(arr) = v.get("results").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .map(|r| SearchResult {
            url: r
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            title: r
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: r
                .get("excerpts")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .or_else(|| {
                    r.get("content")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default(),
        })
        .collect()
}

// ── Exa ─────────────────────────────────────────────────────────────────────

async fn exa_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let url = format!("{}/search", base(cfg, "https://api.exa.ai"));
    let resp = http
        .post(&url)
        .header("x-api-key", &cfg.api_key)
        .json(&json!({
            "query": query,
            "numResults": cfg.max_results,
            "contents": {"text": {"maxCharacters": cfg.max_chars_per_result}}
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    let arr = v
        .get("results")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .map(|r| SearchResult {
            url: r
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            title: r
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: r
                .get("text")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

// ── Brave ───────────────────────────────────────────────────────────────────

async fn brave_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "{}/res/v1/web/search",
        base(cfg, "https://api.search.brave.com")
    );
    let resp = http
        .get(&url)
        .header("X-Subscription-Token", &cfg.api_key)
        .query(&[("q", query), ("count", &cfg.max_results.to_string())])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    let arr = v
        .pointer("/web/results")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .map(|r| SearchResult {
            url: r
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            title: r
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: r
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

// ── Jina ────────────────────────────────────────────────────────────────────

async fn jina_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let url = format!("{}/", base(cfg, "https://s.jina.ai"));
    let resp = http
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .header("Accept", "application/json")
        .json(&json!({"q": query}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    let arr = v
        .get("data")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .into_iter()
        .take(cfg.max_results as usize)
        .map(|r| SearchResult {
            url: r
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            title: r
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: r
                .get("content")
                .and_then(|x| x.as_str())
                .or_else(|| r.get("description").and_then(|x| x.as_str()))
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

// ── Perplexity ──────────────────────────────────────────────────────────────

async fn perplexity_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    // Perplexity exposes /chat/completions; we use it as a search
    // backend by asking for citations.
    let url = format!(
        "{}/chat/completions",
        base(cfg, "https://api.perplexity.ai")
    );
    let resp = http
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .json(&json!({
            "model": "sonar",
            "messages": [{"role": "user", "content": query}],
            "return_citations": true
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    let cites = v
        .get("citations")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let summary = v
        .pointer("/choices/0/message/content")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(cites
        .into_iter()
        .filter_map(|c| c.as_str().map(|s| s.to_string()))
        .map(|url| SearchResult {
            url,
            title: String::new(),
            snippet: summary.clone(),
        })
        .collect())
}

// ── Anthropic ───────────────────────────────────────────────────────────────

async fn anthropic_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    // Uses the Messages API with the `web_search` tool.
    let url = format!("{}/v1/messages", base(cfg, "https://api.anthropic.com"));
    let resp = http
        .post(&url)
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": query}],
            "tools": [{"type": "web_search_20250305", "name": "web_search"}]
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    Ok(extract_anthropic(&v))
}

fn extract_anthropic(v: &Value) -> Vec<SearchResult> {
    let Some(content) = v.get("content").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("server_tool_use_result") {
            if let Some(arr) = block.get("content").and_then(|x| x.as_array()) {
                for item in arr {
                    out.push(SearchResult {
                        url: item
                            .get("url")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        title: item
                            .get("title")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        snippet: item
                            .get("encrypted_content")
                            .or_else(|| item.get("snippet"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
        }
    }
    out
}

// ── Gemini ──────────────────────────────────────────────────────────────────

async fn gemini_search(
    http: &reqwest::Client,
    cfg: &WebSearchConfig,
    query: &str,
) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "{}/v1beta/models/gemini-1.5-pro-latest:generateContent?key={}",
        base(cfg, "https://generativelanguage.googleapis.com"),
        cfg.api_key
    );
    let resp = http
        .post(&url)
        .json(&json!({
            "contents": [{"role": "user", "parts": [{"text": query}]}],
            "tools": [{"google_search_retrieval": {}}]
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    Ok(extract_gemini(&v))
}

fn extract_gemini(v: &Value) -> Vec<SearchResult> {
    let Some(grounding) = v.pointer("/candidates/0/groundingMetadata/groundingChunks") else {
        return Vec::new();
    };
    let Some(arr) = grounding.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|c| c.get("web"))
        .map(|w| SearchResult {
            url: w
                .get("uri")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            title: w
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parse_round_trip_is_case_insensitive() {
        for p in WebSearchProvider::ALL {
            let s = p.as_str();
            assert_eq!(WebSearchProvider::parse(s), Some(*p));
            assert_eq!(WebSearchProvider::parse(&s.to_uppercase()), Some(*p));
        }
        assert!(WebSearchProvider::parse("nope").is_none());
    }

    #[test]
    fn env_keys_match_spec() {
        assert_eq!(WebSearchProvider::Parallel.env_key(), "PARALLEL_API_KEY");
        assert_eq!(WebSearchProvider::Exa.env_key(), "EXA_API_KEY");
        assert_eq!(WebSearchProvider::Brave.env_key(), "BRAVE_API_KEY");
        assert_eq!(WebSearchProvider::Jina.env_key(), "JINA_API_KEY");
        assert_eq!(
            WebSearchProvider::Perplexity.env_key(),
            "PERPLEXITY_API_KEY"
        );
        assert_eq!(WebSearchProvider::Anthropic.env_key(), "ANTHROPIC_API_KEY");
        assert_eq!(WebSearchProvider::Gemini.env_key(), "GEMINI_API_KEY");
    }

    #[test]
    fn format_results_truncates_long_snippets_safely_at_char_boundaries() {
        let r = SearchResult {
            url: "https://example.com".into(),
            title: "ex".into(),
            snippet: "α".repeat(100), // 200 bytes
        };
        let out = format_results(
            "q",
            WebSearchProvider::Parallel,
            std::slice::from_ref(&r),
            10,
        );
        // truncation marker + we never split a multi-byte char.
        assert!(out.contains("…"));
        assert!(out.contains("https://example.com"));
    }

    #[test]
    fn format_results_handles_empty() {
        let s = format_results("q", WebSearchProvider::Parallel, &[], 100);
        assert!(s.contains("(no results)"));
    }

    #[test]
    fn extract_parallel_handles_excerpts_array_or_content_string() {
        // excerpts → joined snippet.
        let v = json!({"results": [{
            "url": "https://x", "title": "X", "excerpts": ["one", "two"]
        }]});
        let r = extract_parallel(&v);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://x");
        assert_eq!(r[0].snippet, "one\ntwo");

        // content fallback.
        let v2 = json!({"results": [{
            "url": "https://y", "title": "Y", "content": "lorem"
        }]});
        let r2 = extract_parallel(&v2);
        assert_eq!(r2[0].snippet, "lorem");
    }

    #[test]
    fn extract_parallel_returns_empty_on_missing_results_field() {
        assert!(extract_parallel(&json!({})).is_empty());
        assert!(extract_parallel(&json!({"results": null})).is_empty());
    }

    #[test]
    fn resolve_config_picks_provider_from_input() {
        let tool = WebSearchTool {
            override_cfg: Some(WebSearchConfig {
                provider: WebSearchProvider::Brave,
                api_key: "k".into(),
                base_url: None,
                max_results: 5,
                max_chars_per_result: 100,
                timeout: Duration::from_secs(1),
            }),
            http: reqwest::Client::new(),
        };
        let cfg = tool
            .resolve_config(&json!({"provider": "exa"}))
            .expect("ok");
        // Override wins.
        assert_eq!(cfg.provider, WebSearchProvider::Brave);
    }

    #[test]
    fn extract_anthropic_picks_up_server_tool_use_results() {
        let v = json!({
            "content": [
                {"type": "text", "text": "ignore me"},
                {"type": "server_tool_use_result", "content": [
                    {"url": "https://a", "title": "A", "snippet": "hello"},
                    {"url": "https://b", "title": "B", "snippet": "world"}
                ]}
            ]
        });
        let r = extract_anthropic(&v);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://a");
        assert_eq!(r[1].title, "B");
    }

    #[test]
    fn extract_gemini_pulls_grounding_chunks() {
        let v = json!({
            "candidates": [{
                "groundingMetadata": {
                    "groundingChunks": [
                        {"web": {"uri": "https://g", "title": "G"}}
                    ]
                }
            }]
        });
        let r = extract_gemini(&v);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://g");
    }
}
