//! Wire types + framing for the host↔guest `web_search` vsock channel.
//!
//! Per RFD 0023 §"web_search via vsock proxy", `web_search` is a guest
//! tool whose handler proxies the call out over a per-VM vsock channel
//! to the host. The host invokes the real `WebSearchTool` with its own
//! `AuthStorage` (so the EXA / Brave / etc. API key never enters the
//! guest), then ships the result back.
//!
//! v1 transport: newline-delimited JSON, one request per connection
//! (no multiplexing). The guest opens vsock(2, `VSOCK_SEARCH_PORT`)
//! per call, writes one [`WebSearchRequest`] line, reads one
//! [`WebSearchResponse`] line, closes. Idle channels do not exist —
//! the worker holds no long-lived fd, which keeps the FD-inheritance
//! hygiene story simple (per RFD §"FD-inheritance hygiene").

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, AsyncRead};

/// Vsock port the host listens on for `web_search` proxy requests.
/// Per RFD 0023 §"web_search via vsock proxy" the canonical port is
/// 5003. The guest opens vsock(`HOST_CID`, `VSOCK_SEARCH_PORT`) per
/// call.
pub const VSOCK_SEARCH_PORT: u32 = 5003;

/// Vsock CID for the host. Always 2 per the vsock spec.
pub const HOST_CID: u32 = 2;

/// Protocol version. Bumped on incompatible wire changes; the host's
/// handler refuses to serve a request whose `proto_version` doesn't
/// match.
pub const CURRENT_PROTO_VERSION: u32 = 1;

/// Default per-line cap. Generous because search results can carry
/// summaries that approach the 256 KiB tool-output cap.
pub const DEFAULT_MAX_LINE_BYTES: usize = 1024 * 1024;

/// One search request, guest → host.
///
/// The shape mirrors the host's `WebSearchTool` input but is decoupled
/// from `pi_tool_types::ToolRequest` so this crate stays
/// transport-only and pulls no `pi-ai` / `pi-tools` deps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchRequest {
    pub proto_version: u32,
    /// Correlation id; the host echoes it back in the response so the
    /// guest can verify channel ordering on (future) multiplexed
    /// transports.
    pub call_id: String,
    /// Search query.
    pub query: String,
    /// Optional provider override (`exa`, `brave`, `jina`, …). When
    /// `None` the host picks per its `WebSearchProvider::default()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional max-results hint. The host clamps to its own ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u32>,
}

/// One search response, host → guest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchResponse {
    pub proto_version: u32,
    pub call_id: String,
    /// Pretty-printed result text (matches `ToolResult::model_output`
    /// shape). On success: search summary. On failure: empty plus
    /// `error` set.
    pub result_text: String,
    /// `Some(_)` on failure (no upstream key, upstream HTTP error,
    /// proto version mismatch, etc.). `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Errors during framing — distinct from upstream search failures
/// (which surface as `WebSearchResponse.error`).
#[derive(Debug, Error)]
pub enum FramingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("frame exceeds max_line_bytes={cap}: got {len}")]
    LineTooLong { len: usize, cap: usize },
    #[error("connection closed before frame received")]
    UnexpectedEof,
}

/// Read one newline-delimited JSON `WebSearchRequest` from `r`,
/// rejecting frames longer than `cap`.
pub async fn read_request<R: AsyncRead + Unpin>(
    r: &mut BufReader<R>,
    cap: usize,
) -> Result<WebSearchRequest, FramingError> {
    let line = read_line_capped(r, cap).await?;
    let req: WebSearchRequest = serde_json::from_str(&line)?;
    Ok(req)
}

/// Read one newline-delimited JSON `WebSearchResponse` from `r`,
/// rejecting frames longer than `cap`.
pub async fn read_response<R: AsyncRead + Unpin>(
    r: &mut BufReader<R>,
    cap: usize,
) -> Result<WebSearchResponse, FramingError> {
    let line = read_line_capped(r, cap).await?;
    let resp: WebSearchResponse = serde_json::from_str(&line)?;
    Ok(resp)
}

async fn read_line_capped<R: AsyncRead + Unpin>(
    r: &mut BufReader<R>,
    cap: usize,
) -> Result<String, FramingError> {
    let mut buf = String::new();
    // BufReader::read_line doesn't enforce a cap; we re-implement to
    // bound memory.
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            if buf.is_empty() {
                return Err(FramingError::UnexpectedEof);
            }
            return Ok(buf);
        }
        let nl = chunk.iter().position(|&b| b == b'\n');
        match nl {
            Some(idx) => {
                if buf.len() + idx > cap {
                    return Err(FramingError::LineTooLong {
                        len: buf.len() + idx,
                        cap,
                    });
                }
                buf.push_str(std::str::from_utf8(&chunk[..idx]).map_err(|e| {
                    FramingError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                })?);
                let consume = idx + 1;
                r.consume(consume);
                return Ok(buf);
            }
            None => {
                if buf.len() + chunk.len() > cap {
                    return Err(FramingError::LineTooLong {
                        len: buf.len() + chunk.len(),
                        cap,
                    });
                }
                buf.push_str(std::str::from_utf8(chunk).map_err(|e| {
                    FramingError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                })?);
                let n = chunk.len();
                r.consume(n);
            }
        }
    }
}

/// Write one `WebSearchRequest` followed by `\n`.
pub async fn write_request<W: AsyncWrite + Unpin>(
    w: &mut W,
    req: &WebSearchRequest,
) -> Result<(), FramingError> {
    let mut s = serde_json::to_string(req)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// Write one `WebSearchResponse` followed by `\n`.
pub async fn write_response<W: AsyncWrite + Unpin>(
    w: &mut W,
    resp: &WebSearchResponse,
) -> Result<(), FramingError> {
    let mut s = serde_json::to_string(resp)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn round_trip_request() {
        let req = WebSearchRequest {
            proto_version: CURRENT_PROTO_VERSION,
            call_id: "abc".into(),
            query: "what is rust".into(),
            provider: Some("exa".into()),
            max_results: Some(5),
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).await.unwrap();
        let mut r = BufReader::new(Cursor::new(buf));
        let got = read_request(&mut r, DEFAULT_MAX_LINE_BYTES).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn round_trip_response_ok() {
        let resp = WebSearchResponse {
            proto_version: CURRENT_PROTO_VERSION,
            call_id: "abc".into(),
            result_text: "result body".into(),
            error: None,
        };
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).await.unwrap();
        let mut r = BufReader::new(Cursor::new(buf));
        let got = read_response(&mut r, DEFAULT_MAX_LINE_BYTES).await.unwrap();
        assert_eq!(got, resp);
    }

    #[tokio::test]
    async fn round_trip_response_err() {
        let resp = WebSearchResponse {
            proto_version: CURRENT_PROTO_VERSION,
            call_id: "abc".into(),
            result_text: String::new(),
            error: Some("missing API key".into()),
        };
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).await.unwrap();
        let mut r = BufReader::new(Cursor::new(buf));
        let got = read_response(&mut r, DEFAULT_MAX_LINE_BYTES).await.unwrap();
        assert_eq!(got, resp);
    }

    #[tokio::test]
    async fn frame_cap_rejects_oversize() {
        let req = WebSearchRequest {
            proto_version: CURRENT_PROTO_VERSION,
            call_id: "x".into(),
            query: "q".repeat(2000),
            provider: None,
            max_results: None,
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).await.unwrap();
        let mut r = BufReader::new(Cursor::new(buf));
        let err = read_request(&mut r, 1024).await.unwrap_err();
        match err {
            FramingError::LineTooLong { cap: 1024, .. } => {}
            other => panic!("expected LineTooLong, got {other}"),
        }
    }

    #[tokio::test]
    async fn unexpected_eof_on_empty_input() {
        let mut r = BufReader::new(Cursor::new(Vec::<u8>::new()));
        let err = read_request(&mut r, 1024).await.unwrap_err();
        assert!(matches!(err, FramingError::UnexpectedEof));
    }
}
