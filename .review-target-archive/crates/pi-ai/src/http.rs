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
//! ## Idle-stream timeout
//!
//! [`streaming_idle_timeout`] returns the max time a streaming provider
//! is allowed to go silent between SSE events before the read loop
//! surfaces an error. A peer that ACKs TCP segments but never sends a
//! frame (real-world cause: gpt-5.4 with `thinking=xhigh` got into a
//! state on 2026-05-03 where the Responses connection stayed
//! ESTABLISHED with empty queues for 58 minutes after the 14th tool
//! turn — `tcp_keepalive` doesn't help since the peer is alive) gets
//! caught here by wrapping each `next().await` in `tokio::time::timeout`.
//! Default 120s, override via `PI_STREAMING_IDLE_TIMEOUT_SECS=N` (set
//! to a generous value in CI).

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

/// Maximum interval an SSE/streaming provider is allowed to go silent
/// between events before the read loop must surface an error. Wrap
/// each `Stream::next().await` in `tokio::time::timeout` with this
/// value to make a peer that ACKs but never sends fail fast instead of
/// parking the agent in `epoll_pwait` forever.
///
/// Default 120s. Overridable via `PI_STREAMING_IDLE_TIMEOUT_SECS=<n>`
/// (parsed as `u64`; 0 disables the timeout entirely — only useful in
/// tests that intentionally exercise long silences). An unparseable
/// value is treated as the default.
pub fn streaming_idle_timeout() -> Duration {
    const DEFAULT_SECS: u64 = 120;
    let secs = std::env::var("PI_STREAMING_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise env-var probing — Rust tests run in parallel and
    /// `set_var` is process-global. A `Mutex` keeps the three test
    /// cases below from interleaving each other's overrides.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn idle_timeout_defaults_to_120s_when_unset() {
        let _g = env_lock();
        std::env::remove_var("PI_STREAMING_IDLE_TIMEOUT_SECS");
        assert_eq!(streaming_idle_timeout(), Duration::from_secs(120));
    }

    #[test]
    fn idle_timeout_respects_env_override() {
        let _g = env_lock();
        std::env::set_var("PI_STREAMING_IDLE_TIMEOUT_SECS", "30");
        assert_eq!(streaming_idle_timeout(), Duration::from_secs(30));
        std::env::remove_var("PI_STREAMING_IDLE_TIMEOUT_SECS");
    }

    #[test]
    fn idle_timeout_falls_back_on_unparseable() {
        let _g = env_lock();
        std::env::set_var("PI_STREAMING_IDLE_TIMEOUT_SECS", "not-a-number");
        assert_eq!(streaming_idle_timeout(), Duration::from_secs(120));
        std::env::remove_var("PI_STREAMING_IDLE_TIMEOUT_SECS");
    }
}
