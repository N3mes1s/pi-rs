//! JSON-RPC stdio transport for an LSP server (D1.transport).
//!
//! Spawns a language-server child process (any binary that speaks LSP
//! over stdin/stdout) and gives the rest of the module a typed interface
//! for `initialize` / requests / notifications. The wire format is the
//! standard LSP `Content-Length: N\r\n\r\n<utf-8 json>` framing.
//!
//! Internals:
//!
//! * **Writer side** — we own `child.stdin` directly behind a tokio
//!   `Mutex` so concurrent `send_request` / `send_notification` calls
//!   don't interleave bytes on the wire.
//! * **Reader side** — a background task reads framed messages off
//!   `child.stdout` until EOF. Responses (those carrying `id`) are
//!   matched against a `pending` map and delivered to a `oneshot`
//!   channel. Server-originated requests/notifications (those without a
//!   matching id) are pushed onto an inbound MPSC for higher layers
//!   (engine.rs) to drain — this lets the engine surface
//!   `textDocument/publishDiagnostics` later without re-touching the
//!   transport.
//! * **IDs** are monotonic `i64`s starting at 1, matching what
//!   real-world LSP servers expect.
//!
//! The transport is intentionally test-friendly: there is no mention of
//! `rust-analyzer`/`pyright`/etc. anywhere — the test suite spawns a
//! tiny Python script that speaks just enough LSP to exercise framing
//! and id correlation.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

/// All errors the transport can surface.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("spawning language server failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("language server stdin/stdout was not piped")]
    NoPipes,
    #[error("io error talking to language server: {0}")]
    Io(#[source] std::io::Error),
    #[error("malformed LSP frame: {0}")]
    Frame(String),
    #[error("language server returned an error: code={code}, message={message}")]
    Rpc { code: i64, message: String },
    #[error("response missing for request id {0} (server closed pipe)")]
    Cancelled(i64),
    #[error("json (de)serialisation: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, TransportError>;

/// One in-flight request: the matching response goes through this oneshot.
type PendingMap = Mutex<HashMap<i64, oneshot::Sender<RpcResponse>>>;

/// Either side of the JSON-RPC `result | error` envelope.
#[derive(Debug)]
struct RpcResponse {
    result: std::result::Result<Value, RpcError>,
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

/// Server-originated message (request *from* the server, or a
/// notification). Pushed on the inbound MPSC so higher layers can
/// observe `publishDiagnostics`, `window/logMessage`, etc.
#[derive(Debug, Clone)]
pub struct ServerMessage {
    pub method: String,
    pub params: Value,
    /// `Some(id)` if the server expects a response, `None` for a
    /// notification. The transport does *not* answer server-side
    /// requests on its own; the engine layer is responsible.
    pub id: Option<Value>,
}

/// Live language-server connection.
///
/// Drop the value to terminate the child (we send SIGKILL — language
/// servers are notoriously bad at shutting down on `exit` notifications,
/// and we don't want test runs to leak).
pub struct LspClient {
    stdin: Mutex<ChildStdin>,
    pending: Arc<PendingMap>,
    next_id: AtomicI64,
    /// Reader task — kept so `Drop` can abort it.
    reader: Option<JoinHandle<()>>,
    child: Option<Child>,
    /// Inbound server→client requests/notifications. `None` once
    /// [`LspClient::take_inbound`] has been called.
    inbound: Mutex<Option<mpsc::Receiver<ServerMessage>>>,
}

impl LspClient {
    /// Spawn a language server with the given argv (must be non-empty).
    /// Stdin/stdout are piped; stderr is inherited so test logs surface
    /// in `cargo test -- --nocapture`.
    pub async fn spawn(argv: &[&str]) -> Result<Self> {
        if argv.is_empty() {
            return Err(TransportError::Frame("empty argv".into()));
        }
        let mut cmd = Command::new(argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(TransportError::Spawn)?;
        let stdin = child.stdin.take().ok_or(TransportError::NoPipes)?;
        let stdout = child.stdout.take().ok_or(TransportError::NoPipes)?;
        let pending: Arc<PendingMap> = Arc::new(Mutex::new(HashMap::new()));
        let (in_tx, in_rx) = mpsc::channel::<ServerMessage>(64);
        let reader = tokio::spawn(reader_loop(stdout, pending.clone(), in_tx));
        Ok(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            reader: Some(reader),
            child: Some(child),
            inbound: Mutex::new(Some(in_rx)),
        })
    }

    /// Take ownership of the inbound channel. Can be called at most
    /// once; later calls return `None`. The engine layer parks a task
    /// on this receiver to surface `publishDiagnostics` etc.
    pub async fn take_inbound(&self) -> Option<mpsc::Receiver<ServerMessage>> {
        self.inbound.lock().await.take()
    }

    /// Send a request and await its response, deserialised into `R`.
    pub async fn send_request<P, R>(&self, method: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": serde_json::to_value(params)?,
        });
        if let Err(e) = self.write_frame(&msg).await {
            // make sure we don't leak a pending entry
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        let resp = rx.await.map_err(|_| TransportError::Cancelled(id))?;
        match resp.result {
            Ok(v) => Ok(serde_json::from_value(v)?),
            Err(e) => Err(TransportError::Rpc {
                code: e.code,
                message: e.message,
            }),
        }
    }

    /// Send a notification — no id, no response.
    pub async fn send_notification<P: Serialize>(&self, method: &str, params: P) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": serde_json::to_value(params)?,
        });
        self.write_frame(&msg).await
    }

    /// Standard LSP three-step handshake:
    ///   1. client → server `initialize` (request)
    ///   2. server → client `InitializeResult` (response)
    ///   3. client → server `initialized` (notification)
    ///
    /// `root_uri` is typically `file:///path/to/cwd`. Returns the raw
    /// `InitializeResult` so callers can inspect server capabilities.
    pub async fn initialize(&self, root_uri: &str) -> Result<Value> {
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "pi-rs", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": false },
                    "hover": { "contentFormat": ["plaintext"] },
                    "definition": { "linkSupport": false },
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": false },
                    "rename": {},
                    "codeAction": {},
                },
                "workspace": { "workspaceFolders": false },
            },
        });
        let result: Value = self.send_request("initialize", params).await?;
        self.send_notification("initialized", json!({})).await?;
        Ok(result)
    }

    async fn write_frame(&self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut guard = self.stdin.lock().await;
        guard
            .write_all(header.as_bytes())
            .await
            .map_err(TransportError::Io)?;
        guard.write_all(&body).await.map_err(TransportError::Io)?;
        guard.flush().await.map_err(TransportError::Io)?;
        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Some(h) = self.reader.take() {
            h.abort();
        }
        // `kill_on_drop(true)` was set at spawn time, so dropping the
        // Child suffices — but be explicit for clarity.
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

/// Read framed LSP messages off `stdout` until EOF, dispatching each
/// response to its `pending` oneshot or pushing server-originated
/// messages onto `inbound`.
async fn reader_loop(
    stdout: ChildStdout,
    pending: Arc<PendingMap>,
    inbound: mpsc::Sender<ServerMessage>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_frame(&mut reader).await {
            Ok(Some(bytes)) => {
                if let Err(e) = dispatch(&bytes, &pending, &inbound).await {
                    tracing::warn!(target: "lsp", "dispatch error: {}", e);
                }
            }
            Ok(None) => break, // clean EOF
            Err(e) => {
                tracing::warn!(target: "lsp", "read error: {}", e);
                break;
            }
        }
    }
    // Server is gone — fail any still-pending requests so their callers
    // don't hang forever.
    let mut p = pending.lock().await;
    p.clear();
}

/// Read a single `Content-Length: N\r\n\r\n<body>` frame, consuming
/// extra header lines (we honour `Content-Length` and ignore the rest,
/// which matches the LSP spec). Returns `Ok(None)` on a clean EOF
/// observed *before* any header bytes.
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    use tokio::io::AsyncBufReadExt;

    let mut content_length: Option<usize> = None;
    let mut saw_any = false;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(TransportError::Io)?;
        if n == 0 {
            // EOF
            if saw_any {
                return Err(TransportError::Frame("EOF mid-header".into()));
            }
            return Ok(None);
        }
        saw_any = true;
        // header section ends at a bare CRLF (or LF)
        if line == "\r\n" || line == "\n" {
            break;
        }
        // tolerate trailing \r\n or \n
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            let v: usize = rest
                .trim()
                .parse()
                .map_err(|_| TransportError::Frame(format!("bad Content-Length: {trimmed}")))?;
            content_length = Some(v);
        }
        // other headers (Content-Type, etc.) ignored.
    }
    let len = content_length
        .ok_or_else(|| TransportError::Frame("missing Content-Length header".into()))?;
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .await
        .map_err(TransportError::Io)?;
    Ok(Some(buf))
}

async fn dispatch(
    bytes: &[u8],
    pending: &Arc<PendingMap>,
    inbound: &mpsc::Sender<ServerMessage>,
) -> Result<()> {
    let v: Value = serde_json::from_slice(bytes)?;
    let id = v.get("id").cloned();
    let method = v.get("method").and_then(|m| m.as_str()).map(String::from);
    match (id, method) {
        // Response to one of our requests (has id, no method).
        (Some(id_v), None) => {
            let id_i = id_v
                .as_i64()
                .ok_or_else(|| TransportError::Frame(format!("non-integer id: {id_v}")))?;
            let resp = if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("(no message)")
                    .to_string();
                RpcResponse {
                    result: Err(RpcError { code, message }),
                }
            } else {
                let result = v.get("result").cloned().unwrap_or(Value::Null);
                RpcResponse { result: Ok(result) }
            };
            if let Some(tx) = pending.lock().await.remove(&id_i) {
                let _ = tx.send(resp);
            } else {
                tracing::warn!(target: "lsp", "unsolicited response id={id_i}");
            }
        }
        // Server-side request (id + method) or notification (method only).
        (id_opt, Some(method)) => {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            let _ = inbound
                .send(ServerMessage {
                    method,
                    params,
                    id: id_opt,
                })
                .await;
        }
        _ => {
            return Err(TransportError::Frame(format!(
                "frame is neither request, response, nor notification: {v}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Read a single frame out of an in-memory buffer — pure framing
    /// test, no subprocess required.
    #[tokio::test]
    async fn read_frame_parses_content_length_and_body() {
        let raw = b"Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}";
        let mut r = BufReader::new(Cursor::new(raw.to_vec()));
        let frame = read_frame(&mut r).await.unwrap().expect("frame");
        assert_eq!(frame, br#"{"jsonrpc":"2.0"}"#);
    }

    #[tokio::test]
    async fn read_frame_tolerates_extra_headers() {
        let raw = b"Content-Length: 2\r\nContent-Type: utf-8\r\nX-Foo: bar\r\n\r\n{}";
        let mut r = BufReader::new(Cursor::new(raw.to_vec()));
        let frame = read_frame(&mut r).await.unwrap().expect("frame");
        assert_eq!(frame, b"{}");
    }

    #[tokio::test]
    async fn read_frame_returns_none_at_clean_eof() {
        let raw: &[u8] = b"";
        let mut r = BufReader::new(Cursor::new(raw.to_vec()));
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_frame_rejects_missing_content_length() {
        let raw = b"X-Foo: bar\r\n\r\n{}";
        let mut r = BufReader::new(Cursor::new(raw.to_vec()));
        let err = read_frame(&mut r).await.unwrap_err();
        assert!(matches!(err, TransportError::Frame(_)));
    }

    #[tokio::test]
    async fn read_frame_handles_two_frames_back_to_back() {
        let raw = b"Content-Length: 2\r\n\r\n{}Content-Length: 4\r\n\r\n[42]";
        let mut r = BufReader::new(Cursor::new(raw.to_vec()));
        let a = read_frame(&mut r).await.unwrap().expect("a");
        let b = read_frame(&mut r).await.unwrap().expect("b");
        assert_eq!(a, b"{}");
        assert_eq!(b, b"[42]");
    }

    /// Path to the python fake LSP server checked into the repo.
    fn fake_server_path() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_lsp_server.py");
        p.to_string_lossy().into_owned()
    }

    fn have_python() -> bool {
        Command::new("python3")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .is_ok()
    }

    #[tokio::test]
    async fn full_initialize_handshake_against_fake_server() {
        if !have_python() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let path = fake_server_path();
        let argv = ["python3", path.as_str()];
        let client = LspClient::spawn(&argv).await.expect("spawn");
        let result = client
            .initialize("file:///tmp/test")
            .await
            .expect("initialize");
        // Our fake server echoes a fixed `serverInfo.name`.
        assert_eq!(
            result["serverInfo"]["name"].as_str(),
            Some("fake-lsp-server")
        );
    }

    #[tokio::test]
    async fn id_correlation_dispatches_concurrent_responses() {
        if !have_python() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let path = fake_server_path();
        let argv = ["python3", path.as_str()];
        let client = Arc::new(LspClient::spawn(&argv).await.expect("spawn"));
        client.initialize("file:///tmp/test").await.expect("init");

        // Fire two concurrent requests; the fake server is configured
        // to delay the first one longer than the second so responses
        // arrive out-of-order. Correct correlation means each future
        // resolves with its *own* matching payload.
        let c1 = client.clone();
        let f1 = tokio::spawn(async move {
            let r: Value = c1
                .send_request("test/echo", json!({"tag": "a", "delay_ms": 80}))
                .await
                .unwrap();
            r
        });
        let c2 = client.clone();
        let f2 = tokio::spawn(async move {
            let r: Value = c2
                .send_request("test/echo", json!({"tag": "b", "delay_ms": 10}))
                .await
                .unwrap();
            r
        });
        let r1 = f1.await.unwrap();
        let r2 = f2.await.unwrap();
        assert_eq!(r1["tag"], "a");
        assert_eq!(r2["tag"], "b");
    }

    #[tokio::test]
    async fn rpc_error_is_surfaced_as_transport_error() {
        if !have_python() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let path = fake_server_path();
        let argv = ["python3", path.as_str()];
        let client = LspClient::spawn(&argv).await.expect("spawn");
        client.initialize("file:///tmp/test").await.expect("init");
        let err = client
            .send_request::<_, Value>("test/error", json!({"code": -32601, "message": "nope"}))
            .await
            .unwrap_err();
        match err {
            TransportError::Rpc { code, message } => {
                assert_eq!(code, -32601);
                assert_eq!(message, "nope");
            }
            other => panic!("expected Rpc error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn server_notifications_arrive_on_inbound_channel() {
        if !have_python() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let path = fake_server_path();
        let argv = ["python3", path.as_str()];
        let client = LspClient::spawn(&argv).await.expect("spawn");
        let mut inbound = client.take_inbound().await.expect("inbound");
        client.initialize("file:///tmp/test").await.expect("init");
        // Ask the server to push a notification at us.
        client
            .send_notification(
                "test/push_notification",
                json!({"method": "window/logMessage", "params": {"type": 3, "message": "hello"}}),
            )
            .await
            .unwrap();
        // The server pushes asynchronously; await with a small timeout.
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .expect("notification received in time")
            .expect("channel open");
        assert_eq!(msg.method, "window/logMessage");
        assert!(msg.id.is_none(), "notifications carry no id");
        assert_eq!(msg.params["message"].as_str(), Some("hello"));
    }
}
