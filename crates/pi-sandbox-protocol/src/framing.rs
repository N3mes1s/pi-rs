//! Async JSON-line framing helpers.
//!
//! Both host and guest use these to read / write `ToolRequest`
//! and `ToolResponse` over an `AsyncRead` / `AsyncWrite`. One
//! JSON object per line, `\n`-terminated. Lines must fit in
//! 64 KiB by default (configurable via `read_request_with_max` and
//! `read_response_with_max`). The host should pass
//! `req.max_output_bytes as usize` to `read_response_with_max` so
//! that the negotiated per-call cap is honoured on the response side.

use crate::{ProtocolError, ToolRequest, ToolResponse};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Default max line length. Tool inputs/outputs above this size
/// fail to deserialise. Guest enforces a stricter cap via
/// ToolRequest.max_output_bytes for the response side.
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

/// Read a `ToolResponse` using the fixed `DEFAULT_MAX_LINE_BYTES` cap.
///
/// For production use on the host side, prefer `read_response_with_max`
/// and pass `req.max_output_bytes as usize` so the negotiated per-call
/// limit is honoured.
pub async fn read_response<R>(reader: &mut BufReader<R>) -> Result<ToolResponse, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_response_with_max(reader, DEFAULT_MAX_LINE_BYTES).await
}

/// Read one `ToolResponse` from a buffered AsyncRead with an explicit
/// max line cap.
///
/// The host should call this with `req.max_output_bytes as usize` so
/// that the cap negotiated in the request is enforced on the response
/// frame as well. Returns `ProtocolError::FrameTooLarge` if the response
/// exceeds `max_bytes` before a `\n` is found; returns
/// `ProtocolError::Eof` if the stream closes before a full line arrives.
pub async fn read_response_with_max<R>(
    reader: &mut BufReader<R>,
    max_bytes: usize,
) -> Result<ToolResponse, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let line = bounded_read_line(reader, max_bytes).await?;
    let resp: ToolResponse = serde_json::from_str(&line)?;
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
