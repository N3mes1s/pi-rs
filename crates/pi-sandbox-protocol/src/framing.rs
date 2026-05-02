//! Async JSON-line framing helpers.
//!
//! Both host and guest use these to read / write `ToolRequest`
//! and `ToolResponse` over an `AsyncRead` / `AsyncWrite`. One
//! JSON object per line, `\n`-terminated. Lines must fit in
//! 64 KiB by default (configurable via `read_request_with_max`).

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
pub async fn read_request_with_max<R>(
    reader: &mut BufReader<R>,
    _max_bytes: usize,
) -> Result<ToolRequest, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(ProtocolError::Eof);
    }
    // Strip trailing newline.
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    let req: ToolRequest = serde_json::from_str(trimmed)?;
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

/// Symmetric helper: read a `ToolResponse` (used by the host).
pub async fn read_response<R>(reader: &mut BufReader<R>) -> Result<ToolResponse, ProtocolError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(ProtocolError::Eof);
    }
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    let resp: ToolResponse = serde_json::from_str(trimmed)?;
    Ok(resp)
}
