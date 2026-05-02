//! Async JSON-line framing helpers.
//!
//! Both host and guest use these to read / write `ToolRequest`
//! and `ToolResponse` over an `AsyncRead` / `AsyncWrite`. One
//! JSON object per line, `\n`-terminated. Lines must fit in
//! 64 KiB by default (configurable via `read_request_with_max`).
//!
//! ## Response-side size cap
//!
//! `ToolRequest.max_output_bytes` is a **stdout byte cap** — it
//! limits how many bytes the guest may write to `ToolResponse.stdout`,
//! not the JSON envelope. On the host side, `read_response_with_max`
//! enforces both a JSON-frame cap (default `DEFAULT_MAX_LINE_BYTES`)
//! and a post-parse `stdout` check against the negotiated limit:
//!
//! ```ignore
//! let resp = read_response_with_max(&mut reader,
//!                                    DEFAULT_MAX_LINE_BYTES,
//!                                    req.max_output_bytes as usize).await?;
//! ```
//!
//! A valid response whose `stdout` is exactly `max_output_bytes` will
//! have a JSON frame that is somewhat larger than that value, which is
//! why the two limits must be kept separate.

use crate::{ProtocolError, ToolRequest, ToolResponse};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Default max JSON-frame length for both requests and responses.
/// Frames larger than this (before the `\n`) are rejected with
/// `ProtocolError::FrameTooLarge`.
pub const DEFAULT_MAX_LINE_BYTES: usize = 64 * 1024;

/// Read one `ToolRequest` from a buffered AsyncRead. Reads
/// exactly one line, parses it, validates `proto_version` against
/// `CURRENT_PROTOCOL_VERSION`. Returns `ProtocolError::Eof` if
/// the stream closed before a full line arrived.
pub async fn read_request<R>(reader: &mut BufReader<R>) -> Result<ToolRequest, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_request_with_max(reader, DEFAULT_MAX_LINE_BYTES).await
}

/// Same as `read_request` with an explicit max line cap.
///
/// The cap is enforced incrementally: as soon as `max_bytes + 1`
/// bytes are buffered without a `\n` being found, the read returns
/// `ProtocolError::FrameTooLarge` — no full allocation of the
/// oversized frame is required.
pub async fn read_request_with_max<R>(
    reader: &mut BufReader<R>,
    max_bytes: usize,
) -> Result<ToolRequest, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let line = bounded_read_line(reader, max_bytes).await?;
    let req: ToolRequest = serde_json::from_str(&line)?;
    if req.proto_version != crate::CURRENT_PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch {
            expected: crate::CURRENT_PROTOCOL_VERSION,
            found: req.proto_version,
        });
    }
    Ok(req)
}

/// Write one `ToolResponse` to an AsyncWrite, followed by `\n`.
pub async fn write_response<W>(
    writer: &mut W,
    resp: &ToolResponse,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    let mut bytes = serde_json::to_vec(resp)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Symmetric helper: write a `ToolRequest` (used by the host).
pub async fn write_request<W>(
    writer: &mut W,
    req: &ToolRequest,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    let mut bytes = serde_json::to_vec(req)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Read a `ToolResponse` using the fixed `DEFAULT_MAX_LINE_BYTES` frame cap
/// and no per-call stdout cap. For production host use, prefer
/// `read_response_with_max` to enforce the negotiated `max_output_bytes`.
pub async fn read_response<R>(reader: &mut BufReader<R>) -> Result<ToolResponse, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_response_with_max(reader, DEFAULT_MAX_LINE_BYTES, usize::MAX).await
}

/// Read one `ToolResponse` from a buffered AsyncRead.
///
/// * `frame_max_bytes` — JSON frame cap: `ProtocolError::FrameTooLarge`
///   is returned as soon as the frame exceeds this many bytes before
///   a `\n` is found. Pass `DEFAULT_MAX_LINE_BYTES` (or larger) if you
///   only want the stdout check to govern.
///
/// * `stdout_max_bytes` — negotiated stdout cap from
///   `ToolRequest.max_output_bytes`. After successful parsing, the
///   function checks `resp.stdout.len() <= stdout_max_bytes` and returns
///   `ProtocolError::StdoutTooLarge` if the guest exceeded the limit.
///   Pass `usize::MAX` to skip this check.
///
/// These two limits are kept separate because the JSON envelope adds
/// per-field overhead: a response with exactly `max_output_bytes` of
/// stdout will produce a frame that is larger than `max_output_bytes`.
pub async fn read_response_with_max<R>(
    reader: &mut BufReader<R>,
    frame_max_bytes: usize,
    stdout_max_bytes: usize,
) -> Result<ToolResponse, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let line = bounded_read_line(reader, frame_max_bytes).await?;
    let resp: ToolResponse = serde_json::from_str(&line)?;
    if resp.stdout.len() > stdout_max_bytes {
        return Err(ProtocolError::StdoutTooLarge {
            size: resp.stdout.len(),
            limit: stdout_max_bytes,
        });
    }
    Ok(resp)
}

/// Read one `\n`-terminated line from `reader`, consuming at most
/// `max_bytes` payload bytes (not counting the newline itself).
///
/// Returns `ProtocolError::Eof` if the stream closes before a `\n`
/// is seen (even if some bytes were received).
/// Returns `ProtocolError::FrameTooLarge` if `max_bytes + 1` bytes
/// accumulate before a `\n` is found.
async fn bounded_read_line<R>(
    reader: &mut BufReader<R>,
    max_bytes: usize,
) -> Result<String, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::with_capacity(256.min(max_bytes + 1));

    loop {
        // Peek at whatever is already in the internal buffer.
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            // EOF — stream closed before we saw a '\n'.
            return Err(ProtocolError::Eof);
        }

        // How many bytes can we look at without exceeding the cap?
        // We allow max_bytes payload bytes plus one sentinel byte to
        // detect oversized frames.
        let remaining_capacity = (max_bytes + 1).saturating_sub(buf.len());
        let window = &chunk[..chunk.len().min(remaining_capacity)];

        match window.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                // Found a newline within the capped window.
                buf.extend_from_slice(&window[..pos]);
                // Consume through the newline itself.
                let consume_len = pos + 1;
                reader.consume(consume_len);
                // Strip optional trailing '\r' (CRLF support).
                let payload = buf.strip_suffix(b"\r").unwrap_or(&buf);
                // Strict UTF-8: reject frames with invalid byte sequences.
                let s = String::from_utf8(payload.to_vec())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                return Ok(s);
            }
            None => {
                // No newline in window — absorb and keep reading.
                buf.extend_from_slice(window);
                let consumed = window.len();
                reader.consume(consumed);

                // If we have already buffered more than the limit, bail out.
                if buf.len() > max_bytes {
                    return Err(ProtocolError::FrameTooLarge {
                        size: buf.len(),
                        limit: max_bytes,
                    });
                }
            }
        }
    }
}
