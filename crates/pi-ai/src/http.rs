//! Shared `reqwest::Client` factory with sane defaults for LLM providers.
//!
//! ## Why
//!
//! `reqwest::Client::new()` builds a client with NO timeouts of any kind.
//! For a non-streaming RPC that's annoying-but-survivable; for a streaming
//! LLM endpoint it's a foot-gun: a server that accepts the connection and
//! then goes silent (network glitch, malformed response, route to a model
//! that doesn't exist at the provider but returns 200 + empty body, etc.)
//! parks the agent loop in `epoll_pwait` indefinitely. We hit this on
//! 2026-04-30 with a misconfigured Fireworks model id — pi's implementer
//! subprocess sat at 0% CPU for 65 minutes before we noticed.
//!
//! ## What this client gives you
//!
//!   * `connect_timeout(30s)` — fail fast on a server we can't even reach.
//!   * `tcp_keepalive(30s)` — kernel detects dead-but-not-closed peers
//!     within ~60s instead of waiting forever.
//!   * `pool_idle_timeout(90s)` — bound the size of the idle-conn pool so
//!     we don't accumulate half-dead sockets across long-running orchestrator
//!     campaigns.
//!   * NO overall request timeout — streaming responses can legitimately
//!     run for many minutes (especially with `thinking=high|xhigh`).
//!
//! ## What this client does NOT give you
//!
//! An *idle-stream* timeout: "fail if no SSE event arrives for N seconds".
//! That requires wrapping `bytes_stream()` in a `tokio::time::timeout`
//! per-chunk. It's the right next step but a bigger change; flagged as a
//! TODO. Until that lands, a malformed response that produces 200 OK + an
//! empty (but technically still-open) body will still hang. The `tcp_keepalive`
//! above closes that hole *partially* by detecting peers that have actually
//! gone away; a peer that's there but choosing not to send still wins.

use std::time::Duration;

/// Returns a `reqwest::Client` configured for LLM-streaming workloads.
///
/// Construction can fail (very rare — only when reqwest can't initialise
/// its native TLS backend). Caller decides whether to propagate or fall
/// back to `Client::new()`.
pub fn streaming_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        .build()
}

/// Convenience wrapper that returns the configured client or falls back
/// to `Client::new()` (no timeouts) if the builder fails. Logs the
/// fallback to stderr so an operator who's debugging a stuck agent has
/// a hint that timeouts are NOT in effect.
pub fn streaming_client_or_default() -> reqwest::Client {
    match streaming_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "pi-ai: warning: failed to build streaming reqwest::Client \
                 with timeouts ({e}); falling back to defaults — agent loop \
                 may hang on a silent server"
            );
            reqwest::Client::new()
        }
    }
}
