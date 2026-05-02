//! pi-sandbox-protocol — JSON-line wire protocol for pi-rs's
//! microVM sandbox boundary.
//!
//! Used by both the host (pi-sandbox) and the guest worker
//! (pi-sandbox-worker, A4). One JSON object per direction,
//! `\n`-framed. Carried over a vsock connection in the local
//! microVM case (RFD 0023) and over any AsyncRead/AsyncWrite
//! transport in the remote case (RFD 0026).
//!
//! The protocol is intentionally minimal and version-negotiated.
//! Field renames within an existing version are MAJOR-breaking;
//! optional field additions with `#[serde(default)]` are MINOR-
//! additive. See RFD 0023 v0.4 §3 for the full stability contract.

use serde::{Deserialize, Serialize};

/// Current wire-protocol version. Increment on any breaking change
/// (field rename, semantics change). Optional-field additions do
/// NOT bump this.
pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

/// Default vsock port the guest worker listens on. Host connects
/// to this port to send ToolRequest lines.
pub const VSOCK_DEFAULT_PORT: u32 = 5001;

/// One tool invocation request from host to guest worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    /// Wire-protocol version. Guest checks against
    /// CURRENT_PROTOCOL_VERSION on receive.
    pub proto_version: u32,
    /// Host-allocated id used by the guest for dedup and by the
    /// host to match response to request.
    pub call_id: String,
    /// Tool name (e.g. "read", "edit", "bash").
    pub tool_name: String,
    /// Tool input JSON (whatever shape the tool's spec defines).
    pub tool_input: serde_json::Value,
    /// Cap on the response's stdout size in bytes.
    pub max_output_bytes: u32,
    /// Per-call wall timeout in milliseconds. Guest enforces.
    pub timeout_ms: u32,
}

/// Tool execution response from guest worker to host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolResponse {
    /// Echoes the request's `call_id` so the host can match.
    pub call_id: String,
    /// Tool's stdout (or model-facing output text).
    pub stdout: String,
    /// Tool's stderr — diagnostic only; never fed to the LLM.
    pub stderr: String,
    /// Process exit status, or 0/1 for tools that don't fork.
    pub exit_status: i32,
    /// Time spent inside the guest, in milliseconds.
    pub guest_duration_ms: u32,
    /// Tool-level "this was an error" flag. Distinct from
    /// exit_status because some tools (e.g. read on a missing
    /// file) report errors without spawning a process.
    pub is_error: bool,
}

/// Framing helpers: read one ToolRequest from an AsyncRead, write
/// one ToolResponse to an AsyncWrite. JSON line framing (one
/// JSON object per line, `\n`-terminated).
pub mod framing;

/// Errors that can arise during framing / serialisation.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("protocol version mismatch: expected {expected}, got {found}")]
    VersionMismatch { expected: u32, found: u32 },
    #[error("end of stream")]
    Eof,
}
