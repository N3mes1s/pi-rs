//! `E2bProvider` — remote sandbox backend via the E2B cloud API (RFD 0026).
//!
//! E2B provides managed microVM-as-a-service with a REST API. This provider
//! ships the `pi-sandbox-worker` binary into an E2B sandbox and dispatches
//! tool calls through it using E2B's command execution API.
//!
//! ## Session lifecycle
//!
//! Construction is always cheap and infallible (`with_key`) or validates only
//! the API key (`from_env`). All network activity — sandbox creation, worker
//! upload, file sync — is deferred to the first `execute_tool()` call (lazy
//! session open). This allows tests to construct `E2bProvider` without any
//! environment state or network reachability.
//!
//! ## Auth
//!
//! Key resolution order in `from_auth(auth)`:
//! 1. `E2B_API_KEY` env var
//! 2. `auth.get("e2b")` — key stored in `AuthStorage` (SDK embedders)
//! 3. Fail with `SandboxError::Unavailable`
//!
//! `with_key(key)` takes an explicit key (for tests and SDK embedders that
//! manage credentials themselves).
//!
//! ## Env vars that gate behaviour
//!
//! | Env var | Default | Purpose |
//! |---------|---------|---------|
//! | `E2B_API_KEY` | — | API key (required for from_env) |
//! | `E2B_BASE_URL` | `https://api.e2b.dev` | Override API endpoint |
//! | `E2B_SANDBOX_TIMEOUT_SECS` | `3600` | Sandbox lifetime cap |
//! | `E2B_COMPUTE_RATE_PER_SEC` | `0.000084` | Override published compute rate |
//! | `E2B_UPLOAD_CONCURRENCY` | `8` | File upload parallelism |
//! | `PI_SANDBOX_WORKER_BIN` | — | Path to `pi-sandbox-worker` binary |
//! | `PI_SANDBOX_OFFLINE=1` | — | Refuse all remote activity |

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use pi_ai::AuthStorage;
use pi_tools::ToolContext;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::provider::{SandboxError, SandboxExecution, SandboxProvider};

// Tokio async mutex used as a single-flight lock for lazy session open.
// Held across the entire open sequence (POST /sandboxes + worker upload +
// chmod + SmartSync) so that concurrent first calls serialise rather than
// each creating their own sandbox.
use tokio::sync::Mutex as AsyncMutex;

/// Default E2B API endpoint.
const DEFAULT_BASE_URL: &str = "https://api.e2b.dev";
/// Default sandbox lifetime in seconds.
const DEFAULT_TIMEOUT_SECS: u32 = 3600;
/// Default E2B compute rate (USD/s) as of 2026-05.
const DEFAULT_COMPUTE_RATE: f64 = 0.000084;
/// Default upload concurrency.
const DEFAULT_UPLOAD_CONCURRENCY: usize = 8;

/// Tools rejected before routing to the remote worker.
/// Provider-side prechecks — no changes to individual tool structs.
const PROVIDER_SIDE_UNAVAILABLE: &[&str] = &[
    "web_search",
    "ask",
    "init_experiment",
    "run_experiment",
    "log_experiment",
    "task",
    "todo",
];

/// Configuration for an E2B sandbox session.
///
/// All fields have documented env-var overrides; `E2bConfig::from_env()`
/// reads them automatically.
#[derive(Debug, Clone)]
pub struct E2bConfig {
    /// E2B API key (from `E2B_API_KEY` or explicit).
    pub api_key: String,
    /// API base URL. Default `https://api.e2b.dev`.
    pub base_url: String,
    /// Sandbox lifetime in seconds (E2B `timeout` param). Default 3600.
    pub sandbox_timeout_secs: u32,
    /// Published compute rate in USD/s. Default 0.000084.
    pub compute_rate_per_sec: f64,
    /// Concurrent file-upload limit. Default 8.
    pub upload_concurrency: usize,
}

impl E2bConfig {
    /// Resolve configuration from environment variables.
    ///
    /// Reads `E2B_API_KEY` (required), `E2B_BASE_URL`, `E2B_SANDBOX_TIMEOUT_SECS`,
    /// `E2B_COMPUTE_RATE_PER_SEC`, and `E2B_UPLOAD_CONCURRENCY`.
    ///
    /// Fails with `SandboxError::Unavailable` when `E2B_API_KEY` is absent.
    /// Also fails eagerly when `PI_SANDBOX_OFFLINE=1` is set.
    pub fn from_env() -> Result<Self, SandboxError> {
        if std::env::var("PI_SANDBOX_OFFLINE").as_deref() == Ok("1") {
            return Err(SandboxError::Unavailable(
                "remote sandbox unavailable: PI_SANDBOX_OFFLINE=1".into(),
            ));
        }
        let api_key = std::env::var("E2B_API_KEY").map_err(|_| {
            SandboxError::Unavailable(
                "E2B API key not configured; set E2B_API_KEY env var".into(),
            )
        })?;
        if api_key.trim().is_empty() {
            return Err(SandboxError::Unavailable(
                "E2B API key not configured; E2B_API_KEY is empty".into(),
            ));
        }
        Ok(Self {
            api_key,
            base_url: std::env::var("E2B_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            sandbox_timeout_secs: std::env::var("E2B_SANDBOX_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            compute_rate_per_sec: std::env::var("E2B_COMPUTE_RATE_PER_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_COMPUTE_RATE),
            upload_concurrency: parse_upload_concurrency(),
        })
    }
}

/// Per-session state managed inside the Mutex.
#[derive(Default)]
struct SessionState {
    /// Active E2B sandbox ID (`None` until first `execute_tool` call).
    sandbox_id: Option<String>,
    /// True once the sandbox has been set up (worker uploaded, /work synced).
    ready: bool,
    /// True if host-side flushback failed — all subsequent calls are rejected.
    poisoned: bool,
    /// Error message to return when poisoned.
    poison_msg: Option<String>,
    /// Instant at which the first `execute_tool` call began (for first-row
    /// round_trip_ms measurement that includes setup overhead).
    session_open_start: Option<Instant>,
}

/// Remote sandbox provider backed by the E2B cloud API (RFD 0026).
///
/// Construction is always cheap:
/// - `from_env()` validates the API key from env but makes no network calls.
/// - `with_key(key)` is unconditionally infallible.
///
/// All network activity is deferred to the first `execute_tool()` call.
pub struct E2bProvider {
    config: E2bConfig,
    client: reqwest::Client,
    state: Arc<Mutex<SessionState>>,
    /// Single-flight lock for lazy session open.
    ///
    /// Held (as an async mutex) across the entire open sequence — POST
    /// /sandboxes, worker upload, chmod, SmartSync — so that two concurrent
    /// first `execute_tool()` calls serialise here rather than each creating
    /// their own remote sandbox. After `state.ready == true` the lock is no
    /// longer contested and has zero overhead.
    open_lock: Arc<AsyncMutex<()>>,
}

impl E2bProvider {
    /// Build from an explicit API key.
    ///
    /// Unconditionally infallible: does not check `PI_SANDBOX_OFFLINE` or
    /// `PI_SANDBOX_WORKER_BIN`. Both are checked lazily on the first
    /// `execute_tool()` call. Allows tests to construct `E2bProvider`
    /// without any environment state.
    pub fn with_key(key: String) -> Self {
        let config = E2bConfig {
            api_key: key,
            base_url: std::env::var("E2B_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            sandbox_timeout_secs: std::env::var("E2B_SANDBOX_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            compute_rate_per_sec: std::env::var("E2B_COMPUTE_RATE_PER_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_COMPUTE_RATE),
            upload_concurrency: parse_upload_concurrency(),
        };
        Self {
            config,
            client: build_http_client(),
            state: Arc::new(Mutex::new(SessionState::default())),
            open_lock: Arc::new(AsyncMutex::new(())),
        }
    }

    /// Resolve configuration from env vars and construct the provider.
    ///
    /// Reads `E2B_API_KEY` (required) and optional overrides.
    /// Fails if `E2B_API_KEY` is absent or `PI_SANDBOX_OFFLINE=1`.
    ///
    /// For CLI use and tests that set `E2B_API_KEY` directly. SDK embedders
    /// managing credentials programmatically should prefer `from_auth`.
    pub fn from_env() -> Result<Self, SandboxError> {
        let config = E2bConfig::from_env()?;
        Ok(Self {
            config,
            client: build_http_client(),
            state: Arc::new(Mutex::new(SessionState::default())),
            open_lock: Arc::new(AsyncMutex::new(())),
        })
    }

    /// Resolve the API key from env (`E2B_API_KEY`) or from `auth.get("e2b")`,
    /// in that priority order. Construct the provider if a key is found.
    ///
    /// Fails with `SandboxError::Unavailable` if neither source has a key, or
    /// if `PI_SANDBOX_OFFLINE=1` is set (eager fail-fast for CLI users).
    ///
    /// Worker binary path is NOT resolved here — resolved lazily on the first
    /// `execute_tool()` call. This keeps construction cheap and environment-
    /// agnostic with respect to `PI_SANDBOX_WORKER_BIN`.
    pub fn from_auth(auth: &AuthStorage) -> Result<Self, SandboxError> {
        if std::env::var("PI_SANDBOX_OFFLINE").as_deref() == Ok("1") {
            return Err(SandboxError::Unavailable(
                "remote sandbox unavailable: PI_SANDBOX_OFFLINE=1".into(),
            ));
        }

        // 1. Try E2B_API_KEY env var.
        let api_key = if let Ok(key) = std::env::var("E2B_API_KEY") {
            if !key.trim().is_empty() {
                key
            } else {
                // env var set but empty; fall through to AuthStorage.
                auth.get("e2b")
                    .and_then(|m| match m {
                        pi_ai::AuthMethod::ApiKey { value } if !value.trim().is_empty() => {
                            Some(value)
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        SandboxError::Unavailable(
                            "E2B API key not configured; set E2B_API_KEY env var".into(),
                        )
                    })?
            }
        } else {
            // 2. Fall back to AuthStorage.
            auth.get("e2b")
                .and_then(|m| match m {
                    pi_ai::AuthMethod::ApiKey { value } if !value.trim().is_empty() => {
                        Some(value)
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    SandboxError::Unavailable(
                        "E2B API key not configured; set E2B_API_KEY env var".into(),
                    )
                })?
        };

        let config = E2bConfig {
            api_key,
            base_url: std::env::var("E2B_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            sandbox_timeout_secs: std::env::var("E2B_SANDBOX_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            compute_rate_per_sec: std::env::var("E2B_COMPUTE_RATE_PER_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_COMPUTE_RATE),
            upload_concurrency: parse_upload_concurrency(),
        };
        Ok(Self {
            config,
            client: build_http_client(),
            state: Arc::new(Mutex::new(SessionState::default())),
            open_lock: Arc::new(AsyncMutex::new(())),
        })
    }

    /// Best-effort sandbox deletion on partial session-open failure.
    /// Errors are logged as warnings and ignored (per RFD 0026).
    async fn try_delete_sandbox(&self, sandbox_id: &str) {
        let url = format!("{}/sandboxes/{}", self.config.base_url, sandbox_id);
        if let Err(e) = self
            .client
            .delete(&url)
            .header("X-API-Key", &self.config.api_key)
            .send()
            .await
        {
            warn!(sandbox_id, err = %e, "E2B: best-effort DELETE sandbox failed");
        }
    }

    /// Estimate per-call cost in USD: compute_rate × elapsed_seconds.
    fn estimate_cost(&self, elapsed_ms: u128) -> f64 {
        self.config.compute_rate_per_sec * (elapsed_ms as f64 / 1000.0)
    }
}

/// JSON body for `POST /sandboxes`.
#[derive(Serialize)]
struct CreateSandboxBody {
    #[serde(rename = "templateID")]
    template_id: String,
    timeout: u32,
}

/// JSON response from `POST /sandboxes`.
#[derive(Deserialize)]
struct CreateSandboxResponse {
    #[serde(rename = "sandboxID")]
    sandbox_id: String,
}

/// JSON body for `POST /sandboxes/{id}/commands`.
#[derive(Serialize)]
struct RunCommandBody {
    cmd: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdin: Option<String>,
}

/// JSON response from `POST /sandboxes/{id}/commands`.
#[derive(Deserialize)]
struct RunCommandResponse {
    #[serde(rename = "cmdID")]
    cmd_id: String,
}

/// JSON response from `GET /sandboxes/{id}/commands/{cmd_id}`.
#[derive(Deserialize)]
struct PollCommandResponse {
    finished: bool,
    #[serde(rename = "exitCode")]
    exit_code: Option<i32>,
    stdout: Option<String>,
}

/// ToolResponse shape for parsing worker output.
/// Mirrors `pi_sandbox_protocol::ToolResponse` with `file_writes` as default-empty
/// so we can parse both proto v1 (no file_writes) and v2 (with file_writes).
#[derive(Deserialize)]
struct WorkerToolResponse {
    stdout: String,
    stderr: String,
    exit_status: i32,
    is_error: bool,
    /// File mutations (proto v2, RFD 0026). Absent in v1 responses.
    #[serde(default)]
    file_writes: Vec<WorkerFileWrite>,
}

/// A single file mutation from the worker (mirrors pi_sandbox_protocol::FileWrite).
#[derive(Deserialize)]
struct WorkerFileWrite {
    path: String,
    contents_b64: String,
    mode: u32,
}

impl E2bProvider {
    /// Perform lazy session open: create sandbox, upload worker, sync /work.
    /// Called on the first `execute_tool()` call, inside the lock-free fast path.
    async fn ensure_session_open(
        &self,
        cwd: &std::path::Path,
    ) -> Result<String, SandboxError> {
        // 1. Check PI_SANDBOX_OFFLINE.
        if std::env::var("PI_SANDBOX_OFFLINE").as_deref() == Ok("1") {
            return Err(SandboxError::Unavailable(
                "remote sandbox unavailable: PI_SANDBOX_OFFLINE=1".into(),
            ));
        }

        // 2. Check PI_SANDBOX_WORKER_BIN.
        let worker_bin = std::env::var("PI_SANDBOX_WORKER_BIN").map_err(|_| {
            SandboxError::Unavailable(
                "PI_SANDBOX_WORKER_BIN not set; \
                 pi-sandbox-worker must be built separately and its path set via this env var"
                    .into(),
            )
        })?;
        if !std::path::Path::new(&worker_bin).exists() {
            return Err(SandboxError::Unavailable(format!(
                "PI_SANDBOX_WORKER_BIN={worker_bin} does not exist"
            )));
        }

        // 3. Create the E2B sandbox.
        let create_url = format!("{}/sandboxes", self.config.base_url);
        let create_body = CreateSandboxBody {
            template_id: "base".to_string(),
            timeout: self.config.sandbox_timeout_secs,
        };
        let resp = self
            .client
            .post(&create_url)
            .header("X-API-Key", &self.config.api_key)
            .json(&create_body)
            .send()
            .await
            .map_err(|e| map_reqwest_err(e))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SandboxError::Unavailable(
                "E2B API key invalid or revoked".into(),
            ));
        }
        if status == reqwest::StatusCode::PAYMENT_REQUIRED {
            return Err(SandboxError::BillingError(
                "account suspended or quota exceeded".into(),
            ));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            return Err(SandboxError::RateLimited {
                retry_after_secs: retry_after,
            });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SandboxError::Unavailable(format!(
                "E2B API error: {status} {body}"
            )));
        }
        let sandbox: CreateSandboxResponse = resp
            .json()
            .await
            .map_err(|e| SandboxError::Provider(e.to_string()))?;
        let sandbox_id = sandbox.sandbox_id;

        // 4. Upload the worker binary.
        let upload_result = self
            .upload_worker(&sandbox_id, &worker_bin)
            .await;
        if let Err(e) = upload_result {
            self.try_delete_sandbox(&sandbox_id).await;
            return Err(e);
        }

        // 5. chmod +x the worker.
        let chmod_result = self
            .run_command_sync(
                &sandbox_id,
                vec![
                    "chmod".into(),
                    "+x".into(),
                    "/usr/local/bin/pi-sandbox-worker".into(),
                ],
                None,
                30_000,
            )
            .await;
        if let Err(e) = chmod_result {
            self.try_delete_sandbox(&sandbox_id).await;
            return Err(SandboxError::Provider(format!(
                "session open failed: chmod worker: {e}"
            )));
        }

        // 6. SmartSync cwd → /work (best-effort; errors abort session open).
        let sync_result = self.smart_sync(&sandbox_id, cwd).await;
        if let Err(e) = sync_result {
            self.try_delete_sandbox(&sandbox_id).await;
            return Err(e);
        }

        Ok(sandbox_id)
    }

    /// Upload the worker binary to `/usr/local/bin/pi-sandbox-worker`.
    async fn upload_worker(&self, sandbox_id: &str, worker_bin: &str) -> Result<(), SandboxError> {
        let bytes = tokio::fs::read(worker_bin).await.map_err(|e| {
            SandboxError::Provider(format!("upload failed: read worker binary: {e}"))
        })?;
        let url = format!(
            "{}/sandboxes/{}/files?path=/usr/local/bin/pi-sandbox-worker",
            self.config.base_url, sandbox_id
        );
        let resp = self
            .client
            .post(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(SandboxError::Provider(format!(
                "upload failed: pi-sandbox-worker: {status} {body}"
            )));
        }
        Ok(())
    }

    /// Upload host cwd files to /work in the sandbox using SmartSync.
    ///
    /// Walks the cwd, filters out excluded paths (node_modules, target, etc.
    /// and files > 100 MB), then uploads concurrently.
    async fn smart_sync(
        &self,
        sandbox_id: &str,
        cwd: &std::path::Path,
    ) -> Result<(), SandboxError> {
        use futures::StreamExt;

        let files = collect_upload_files(cwd);
        let concurrency = self.config.upload_concurrency;
        let base_url = self.config.base_url.clone();
        let api_key = self.config.api_key.clone();
        let client = self.client.clone();
        let sandbox_id = sandbox_id.to_string();
        let cwd = cwd.to_path_buf();

        futures::stream::iter(files)
            .map(|path| {
                let base_url = base_url.clone();
                let api_key = api_key.clone();
                let client = client.clone();
                let sandbox_id = sandbox_id.clone();
                let cwd = cwd.clone();
                async move {
                    let rel = path.strip_prefix(&cwd).unwrap_or(&path);
                    // Build the POSIX remote path with explicit forward-slash
                    // separators. `to_string_lossy()` uses the *host* OS path
                    // separator (backslash on Windows), which would produce an
                    // invalid guest path like `/work\src\main.rs`.
                    let posix_rel: String = rel
                        .components()
                        .filter_map(|c| {
                            use std::path::Component;
                            match c {
                                Component::Normal(n) => n.to_str().map(|s| s.to_owned()),
                                _ => None,
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    let remote_path = format!("/work/{posix_rel}");
                    let encoded = percent_encode_path(&remote_path);
                    let url =
                        format!("{base_url}/sandboxes/{sandbox_id}/files?path={encoded}");
                    let bytes = tokio::fs::read(&path).await.map_err(|e| {
                        SandboxError::Provider(format!(
                            "upload failed: {}: {e}",
                            path.display()
                        ))
                    })?;
                    let resp = client
                        .post(&url)
                        .header("X-API-Key", &api_key)
                        .header("Content-Type", "application/octet-stream")
                        .body(bytes)
                        .send()
                        .await
                        .map_err(map_reqwest_err)?;
                    if !resp.status().is_success() {
                        let status = resp.status();
                        return Err(SandboxError::Provider(format!(
                            "upload failed: {}: {status}",
                            rel.display()
                        )));
                    }
                    Ok::<(), SandboxError>(())
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .find(|r| r.is_err())
            .unwrap_or(Ok(()))
    }

    /// Run a command synchronously (create + poll until finished).
    /// Returns the exit code or an error if the poll times out.
    async fn run_command_sync(
        &self,
        sandbox_id: &str,
        cmd: Vec<String>,
        stdin: Option<String>,
        timeout_ms: u64,
    ) -> Result<PollCommandResponse, SandboxError> {
        // Submit the command.
        let cmd_url = format!("{}/sandboxes/{}/commands", self.config.base_url, sandbox_id);
        let body = RunCommandBody { cmd, stdin };
        let resp = self
            .client
            .post(&cmd_url)
            .header("X-API-Key", &self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(SandboxError::Provider(format!(
                "E2B command submit failed: {status} {text}"
            )));
        }
        let run_resp: RunCommandResponse =
            resp.json().await.map_err(|e| SandboxError::Provider(e.to_string()))?;
        let cmd_id = run_resp.cmd_id;

        // Poll until finished or timeout.
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_millis(timeout_ms + 5000);
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(SandboxError::Timeout);
            }
            let poll_url = format!(
                "{}/sandboxes/{}/commands/{}",
                self.config.base_url, sandbox_id, cmd_id
            );
            let poll_resp = self
                .client
                .get(&poll_url)
                .header("X-API-Key", &self.config.api_key)
                .send()
                .await
                .map_err(map_reqwest_err)?;
            if !poll_resp.status().is_success() {
                let status = poll_resp.status();
                return Err(SandboxError::Provider(format!(
                    "E2B command poll failed: {status}"
                )));
            }
            let poll: PollCommandResponse = poll_resp
                .json()
                .await
                .map_err(|e| SandboxError::Provider(e.to_string()))?;
            if poll.finished {
                return Ok(poll);
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }

    /// Execute a tool in the sandbox by running the worker one-shot via stdin.
    /// Returns SandboxExecution and any file_writes from the response.
    async fn dispatch_tool(
        &self,
        sandbox_id: &str,
        tool_name: &str,
        tool_input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<(SandboxExecution, Vec<WorkerFileWrite>), SandboxError> {
        use pi_sandbox_protocol::{ToolRequest, CURRENT_PROTOCOL_VERSION};

        // Extract per-tool timeout from the tool_input JSON (e.g. bash passes
        // `timeout_ms`). Fall back to 120 000 ms (the bash default) so we don't
        // silently cap long-running commands at 60 s. The RFD specifies that
        // `ToolRequest.timeout_ms` is the actual enforced guest timeout.
        //
        // Cap at 600_000 ms (10 minutes) — the same hard ceiling BashTool
        // enforces via `BASH_MAX_TIMEOUT_MS` (pi_tools_core::bash). Without
        // this clamp a caller can pass `timeout_ms = u32::MAX` (~49 days) and
        // the poll loop blocks far past any reasonable deadline.
        let tool_timeout_ms: u32 = tool_input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(120_000)
            .min(600_000) as u32;

        let call_id = format!("e2b-{}", uuid_like());
        let request = ToolRequest {
            proto_version: CURRENT_PROTOCOL_VERSION,
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
            tool_input: tool_input.clone(),
            max_output_bytes: ctx.max_output_bytes as u32,
            timeout_ms: tool_timeout_ms,
        };
        let stdin_line = serde_json::to_string(&request)
            .map_err(|e| SandboxError::Provider(e.to_string()))?
            + "\n";

        let cmd = vec![
            "/usr/local/bin/pi-sandbox-worker".to_string(),
            "--transport".to_string(),
            "stdin".to_string(),
            "--work-dir".to_string(),
            "/work".to_string(),
            "--log-level".to_string(),
            "warn".to_string(),
        ];

        let poll = self
            .run_command_sync(sandbox_id, cmd, Some(stdin_line), tool_timeout_ms as u64)
            .await?;

        let exit_code = poll.exit_code.unwrap_or(1);
        let raw_stdout = poll.stdout.unwrap_or_default();

        // Parse the ToolResponse from stdout.
        match serde_json::from_str::<WorkerToolResponse>(raw_stdout.trim()) {
            Ok(tr) => {
                let exec = SandboxExecution {
                    stdout: tr.stdout,
                    stderr: tr.stderr,
                    exit_status: if tr.is_error && tr.exit_status == 0 { 1 } else { tr.exit_status },
                    round_trip_ms: None, // filled by caller
                    cost_usd: None,      // filled by caller
                };
                Ok((exec, tr.file_writes))
            }
            Err(_) => Err(SandboxError::Provider(format!(
                "worker exited with code {exit_code}; no valid ToolResponse in stdout"
            ))),
        }
    }

    /// Validate a worker-supplied path for host flushback.
    ///
    /// Returns the validated path joined against `cwd`, or a descriptive error
    /// string that the caller can wrap in a session-poison failure.
    ///
    /// Rejects paths that:
    /// - Are absolute (start with `/` or a drive letter on Windows).
    /// - Contain `..` (parent-directory) components.
    ///
    /// As a final defence, asserts the joined path is lexically inside `cwd`.
    /// This covers any edge cases that `Path::components()` does not catch on
    /// exotic host filesystems (e.g. paths that are valid relative paths yet
    /// resolve outside the root when joined).
    fn validate_flushback_path(
        cwd: &std::path::Path,
        raw: &str,
    ) -> Result<std::path::PathBuf, String> {
        use std::path::Component;

        let p = std::path::Path::new(raw);

        for component in p.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => {
                    return Err(format!(
                        "flushback path '{}' is absolute; only relative paths are \
                         permitted for host flushback",
                        raw
                    ));
                }
                Component::ParentDir => {
                    return Err(format!(
                        "flushback path '{}' contains '..' component; paths must not \
                         escape the session directory",
                        raw
                    ));
                }
                Component::CurDir | Component::Normal(_) => {}
            }
        }

        let joined = cwd.join(p);

        // Lexical containment check: after joining, the path must start with
        // `cwd`.  `Path::starts_with` uses component-level matching, so it is
        // not fooled by a suffix that happens to share a string prefix with cwd.
        if !joined.starts_with(cwd) {
            return Err(format!(
                "flushback path '{}' resolves outside the session directory \
                 (joined = {}); rejecting to prevent host-file overwrite",
                raw,
                joined.display()
            ));
        }

        Ok(joined)
    }

    /// Check that no component of `path` (between `cwd` exclusive and the full
    /// `path` inclusive) is a symlink on the host filesystem.
    ///
    /// A pre-existing symlink inside `cwd` (e.g. `link -> /outside`) passes
    /// the lexical `starts_with(cwd)` test in `validate_flushback_path` but
    /// would cause `create_dir_all` / `write` / `rename` to follow it and
    /// potentially write outside the session directory.  We reject that by
    /// inspecting existing components with `symlink_metadata` before touching
    /// any file.
    ///
    /// Components that do not yet exist (i.e. the deepest dirs that
    /// `create_dir_all` will create) are skipped — they cannot be symlinks.
    fn check_no_symlinks_in_parent_chain(
        cwd: &std::path::Path,
        host_path: &std::path::Path,
    ) -> Result<(), String> {
        // Walk from `cwd` down to `host_path` (the file itself, not just parent).
        // We check every component that already exists.
        let mut cursor = cwd.to_path_buf();
        // Build the relative suffix (everything after cwd).
        let rel = match host_path.strip_prefix(cwd) {
            Ok(r) => r,
            Err(_) => return Err(format!(
                "flushback path '{}' is not inside the session directory '{}'",
                host_path.display(), cwd.display()
            )),
        };

        for component in rel.components() {
            cursor.push(component);
            match std::fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!(
                        "flushback path component '{}' is a symlink; \
                         writing through symlinks is not permitted for host flushback \
                         (symlink could point outside the session directory)",
                        cursor.display()
                    ));
                }
                Ok(_) => {} // exists, not a symlink — safe to traverse
                Err(_) => break, // does not exist yet; created by create_dir_all — safe
            }
        }
        Ok(())
    }

    /// Apply file_writes from the worker response to the host cwd (flushback).
    ///
    /// Per RFD 0026: uses atomic temp-write + rename, verifies length after apply.
    /// On any failure, poisons the session and returns an error.
    async fn apply_file_writes(
        &self,
        sandbox_id: &str,
        file_writes: Vec<WorkerFileWrite>,
        cwd: &std::path::Path,
    ) -> Result<(), SandboxError> {
        use base64::Engine as _;
        for fw in file_writes {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&fw.contents_b64)
                .map_err(|e| {
                    let msg = format!(
                        "E2B session desynced after flushback failure on '{}':                          base64 decode error: {e}. Restart the pi session to recover.",
                        fw.path
                    );
                    self.poison_session(sandbox_id, msg.clone());
                    SandboxError::Provider(msg)
                })?;
            // Security: validate the worker-supplied path before joining it to
            // the host cwd.  A malformed or compromised remote response could
            // supply an absolute path or one with ".." components to overwrite
            // arbitrary host files outside the session directory.
            let host_path = match Self::validate_flushback_path(cwd, &fw.path) {
                Ok(p) => p,
                Err(reason) => {
                    let msg = format!(
                        "E2B session desynced after flushback failure on '{}': \
                         {reason}. Restart the pi session to recover.",
                        fw.path
                    );
                    self.poison_session(sandbox_id, msg.clone());
                    return Err(SandboxError::Provider(msg));
                }
            };
            // Security: reject any path whose existing parent chain contains a
            // symlink.  A pre-existing symlink inside cwd (e.g. `link ->
            // /outside`) passes the lexical `starts_with(cwd)` check above but
            // would cause `create_dir_all`/`write`/`rename` to follow it and
            // overwrite host files outside the session directory.
            if let Err(reason) = Self::check_no_symlinks_in_parent_chain(cwd, &host_path) {
                let msg = format!(
                    "E2B session desynced after flushback failure on '{}': \
                     {reason}. Restart the pi session to recover.",
                    fw.path
                );
                self.poison_session(sandbox_id, msg.clone());
                return Err(SandboxError::Provider(msg));
            }
            if let Some(parent) = host_path.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    let msg = format!(
                        "E2B session desynced after flushback failure on '{}':                          could not create parent directory: {e}. Restart the pi session to recover.",
                        fw.path
                    );
                    self.poison_session(sandbox_id, msg.clone());
                    return Err(SandboxError::Provider(msg));
                }
            }
            // Atomic write via temp file + rename.
            let tmp_path = {
                let mut t = host_path.clone();
                let stem = t.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
                t.set_file_name(format!(".{stem}.__e2b_tmp__"));
                t
            };
            if let Err(e) = tokio::fs::write(&tmp_path, &decoded).await {
                let msg = format!(
                    "E2B session desynced after flushback failure on '{}':                      write error: {e}. Restart the pi session to recover.",
                    fw.path
                );
                self.poison_session(sandbox_id, msg.clone());
                return Err(SandboxError::Provider(msg));
            }
            if let Err(e) = tokio::fs::rename(&tmp_path, &host_path).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                let msg = format!(
                    "E2B session desynced after flushback failure on '{}':                      rename error: {e}. Restart the pi session to recover.",
                    fw.path
                );
                self.poison_session(sandbox_id, msg.clone());
                return Err(SandboxError::Provider(msg));
            }
            // Restore the original Unix permission bits from the guest
            // (e.g. preserve +x on scripts). Only on Unix; no-op on other targets.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(fw.mode);
                if let Err(e) = tokio::fs::set_permissions(&host_path, perms).await {
                    let msg = format!(
                        "E2B session desynced after flushback failure on '{}': \
                         could not set permissions (mode={:#o}): {e}. \
                         Restart the pi session to recover.",
                        fw.path, fw.mode
                    );
                    self.poison_session(sandbox_id, msg.clone());
                    return Err(SandboxError::Provider(msg));
                }
            }
            // Verification: read back and check length matches decoded bytes.
            match tokio::fs::metadata(&host_path).await {
                Ok(meta) if meta.len() as usize == decoded.len() => {}
                Ok(meta) => {
                    let msg = format!(
                        "E2B session desynced after flushback failure on '{}'                          (verification mismatch: wrote {} bytes, read back {} bytes).                          Restart the pi session to recover.",
                        fw.path, decoded.len(), meta.len()
                    );
                    self.poison_session(sandbox_id, msg.clone());
                    return Err(SandboxError::Provider(msg));
                }
                Err(e) => {
                    let msg = format!(
                        "E2B session desynced after flushback failure on '{}'                          (verification read failed: {e}). Restart the pi session to recover.",
                        fw.path
                    );
                    self.poison_session(sandbox_id, msg.clone());
                    return Err(SandboxError::Provider(msg));
                }
            }
        }
        Ok(())
    }

    /// Poison the session: record the error message, clear sandbox state,
    /// and best-effort delete the remote sandbox.
    fn poison_session(&self, sandbox_id: &str, msg: String) {
        let id_to_delete = {
            let mut state = self.state.lock().expect("E2bProvider state lock poisoned");
            if state.poisoned {
                return; // already poisoned
            }
            state.poisoned = true;
            state.poison_msg = Some(msg);
            state.ready = false;
            state.sandbox_id.take().unwrap_or_else(|| sandbox_id.to_string())
        };
        // Best-effort DELETE — fire and forget.
        let client = self.client.clone();
        let base_url = self.config.base_url.clone();
        let api_key = self.config.api_key.clone();
        tokio::spawn(async move {
            let url = format!("{base_url}/sandboxes/{id_to_delete}");
            if let Err(e) = client
                .delete(&url)
                .header("X-API-Key", &api_key)
                .send()
                .await
            {
                warn!(sandbox_id = %id_to_delete, err = %e, "E2B: best-effort DELETE after poison failed");
            }
        });
    }
}

#[async_trait]
impl SandboxProvider for E2bProvider {
    fn name(&self) -> &'static str {
        "e2b"
    }

    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        // Provider-side precheck: reject tools that are unavailable in remote.
        if PROVIDER_SIDE_UNAVAILABLE.contains(&tool_name) {
            return Err(SandboxError::Provider(format!(
                "`{tool_name}` is not available in the E2B remote sandbox"
            )));
        }

        // Check for poisoned session.
        {
            let state = self.state.lock().expect("E2bProvider state lock poisoned");
            if state.poisoned {
                let msg = state.poison_msg.clone().unwrap_or_else(|| {
                    "E2B session desynced; restart the pi session to recover.".into()
                });
                return Err(SandboxError::Provider(msg));
            }
        }

        let call_start = Instant::now();

        // Single-flight lazy session open: acquire the open_lock so that two
        // concurrent first calls serialise here rather than each creating their
        // own remote sandbox. After the session is ready the lock is released
        // and all subsequent calls skip straight to the ready branch below.
        let sandbox_id = {
            let _open_guard = self.open_lock.lock().await;

            // Re-check ready under the open_lock (double-check idiom). A prior
            // concurrent caller may have already completed session open by the
            // time we acquired the lock.
            let already_ready = {
                let state = self.state.lock().expect("E2bProvider state lock poisoned");
                state.ready
            };

            if !already_ready {
                // Record the session-open start time (first call only).
                {
                    let mut state =
                        self.state.lock().expect("E2bProvider state lock poisoned");
                    if state.session_open_start.is_none() {
                        state.session_open_start = Some(call_start);
                    }
                }

                // Lazy session open.
                let id = self.ensure_session_open(&ctx.cwd).await?;
                {
                    let mut state =
                        self.state.lock().expect("E2bProvider state lock poisoned");
                    state.sandbox_id = Some(id.clone());
                    state.ready = true;
                }
                id
            } else {
                let state = self.state.lock().expect("E2bProvider state lock poisoned");
                state
                    .sandbox_id
                    .clone()
                    .expect("ready=true but sandbox_id is None")
            }
            // open_guard drops here, releasing the async lock.
        };

        // Dispatch the tool call.
        let (mut exec, file_writes) = self
            .dispatch_tool(&sandbox_id, tool_name, tool_input, ctx)
            .await?;

        // Detect inline-flushback overflow (RFD 0026 §"Size cap and fallback").
        //
        // When a `write`/`edit` tool writes a file > 32 KiB, the worker returns
        // `is_error: true, file_writes: []` with the sentinel message
        // "remote sync error: file too large for inline flushback" in stdout.
        // At that point the remote /work has the new content but the host cwd
        // does not — the session is desynced.  We must poison (fail closed) so
        // subsequent calls surface the divergence rather than silently operating
        // on a stale host view.
        if exec.exit_status != 0
            && file_writes.is_empty()
            && matches!(tool_name, "write" | "edit")
            && exec.stdout.contains("remote sync error: file too large for inline flushback")
        {
            let msg = format!(
                "E2B session desynced after inline-flushback overflow:                  remote /work was written but content exceeds the 32 KiB inline                  transfer limit; host cwd is now stale.                  Restart the pi session to recover.                  (worker: {})",
                exec.stdout
            );
            self.poison_session(&sandbox_id, msg.clone());
            return Err(SandboxError::Provider(msg));
        }

        // Fill in telemetry.
        let elapsed_ms = call_start.elapsed().as_millis();
        exec.round_trip_ms = Some(elapsed_ms.min(u32::MAX as u128) as u32);
        exec.cost_usd = Some(self.estimate_cost(elapsed_ms));

        // Apply file_writes flushback (v1: write/edit tools only, RFD 0026).
        // On any host-side write failure, the session is poisoned.
        if !file_writes.is_empty() {
            self.apply_file_writes(&sandbox_id, file_writes, &ctx.cwd).await?;
        }

        Ok(exec)
    }

    async fn cleanup(&self) -> Result<(), SandboxError> {
        let sandbox_id = {
            let mut state = self.state.lock().expect("E2bProvider state lock poisoned");
            state.ready = false;
            state.sandbox_id.take()
        };
        if let Some(id) = sandbox_id {
            self.try_delete_sandbox(&id).await;
        }
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Per-request timeout for all E2B API calls.
/// Matches the "Network timeout (> 30 s)" entry in RFD 0026 §"Failure modes".
/// Without this, a hung TCP connection silently blocks `run_command_sync`'s
/// poll loop and the tool-call deadline logic is never reached.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Construct the shared reqwest client with a 30-second per-request timeout.
///
/// All E2B API calls (POST /sandboxes, POST .../commands, GET .../commands/{id},
/// DELETE /sandboxes/{id}, and every file-upload POST) use this client.  Without
/// a request timeout a hung TCP connection silently stalls the surrounding poll
/// loop, making tool calls block far past the documented 30-second limit.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("reqwest::Client::builder() failed; this is a bug in reqwest configuration")
}

/// Parse `E2B_UPLOAD_CONCURRENCY`, clamping zero to 1.
///
/// `buffer_unordered(0)` never polls any futures — it hangs forever.
/// A user who sets `E2B_UPLOAD_CONCURRENCY=0` almost certainly means
/// "sequential" (concurrency 1), not "freeze forever".
fn parse_upload_concurrency() -> usize {
    std::env::var("E2B_UPLOAD_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_UPLOAD_CONCURRENCY)
        .max(1) // clamp 0 → 1; buffer_unordered(0) hangs forever
}

/// Map a reqwest error to a `SandboxError`.
fn map_reqwest_err(e: reqwest::Error) -> SandboxError {
    if e.is_timeout() {
        SandboxError::Unavailable("E2B API request timed out".into())
    } else {
        SandboxError::Unavailable(format!("E2B network error: {e}"))
    }
}

/// Percent-encode a file path for use as a query parameter value.
///
/// Uses `percent_encoding::utf8_percent_encode` with a custom set that
/// encodes every byte that is not an unreserved URI character (ALPHA / DIGIT /
/// `-` / `_` / `.` / `~`) as well as `/` (which is legal inside a path
/// segment but must be preserved verbatim here because the `path=` value
/// represents a guest file path). Non-ASCII bytes, `&`, `+`, `%`, `=`, `#`,
/// `?`, space, and all other ASCII special characters are all encoded.
fn percent_encode_path(s: &str) -> String {
    use percent_encoding::{percent_encode, AsciiSet, CONTROLS};
    // Encode everything except unreserved chars (RFC 3986 §2.3) and `/`.
    // The CONTROLS set covers bytes 0x00–0x1F and 0x7F; we extend it to
    // cover all other special/space/non-ASCII bytes.
    const QUERY_COMPONENT: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'&')
        .add(b'+')
        .add(b'=')
        .add(b'?')
        .add(b'@')
        .add(b'[')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');
    percent_encode(s.as_bytes(), QUERY_COMPONENT).to_string()
}

/// Hard exclusion globs for SmartSync (applied in addition to .gitignore).
///
/// These cover large generated directories that are almost never useful to
/// upload and that .gitignore may not always list.
const EXCLUDED_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "dist",
    "build",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    ".gradle",
    ".terraform",
    "vendor",
    "bower_components",
];
const EXCLUDED_EXTS: &[&str] = &["pyc", "class", "o", "so", "dylib"];
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024; // 100 MB

/// Walk `cwd`, apply SmartSync exclusions, return list of regular files to upload.
///
/// Safety properties guaranteed by this function:
/// - **No symlinks followed.** Symlinks are silently skipped so that a
///   symlink in the project root cannot exfiltrate arbitrary host files.
/// - **`.gitignore` respected.** Uses the `ignore` crate which honours
///   `.gitignore`, `.git/info/exclude`, and global gitignore rules, so
///   ignored secrets (e.g. `.env`) are not uploaded.
/// - **Hard exclusion list applied** on top of `.gitignore` for large
///   generated directories that are occasionally not gitignored.
fn collect_upload_files(cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    use ignore::WalkBuilder;

    let mut out = Vec::new();

    let walker = WalkBuilder::new(cwd)
        // Respect .gitignore, .git/info/exclude, global gitignore.
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        // Do NOT follow symlinks — a symlink could point outside the project.
        .follow_links(false)
        // Include hidden files (e.g. .env that isn't gitignored is intentionally
        // visible so the user can inspect; filtering is .gitignore's job).
        .hidden(false)
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip if not a regular file (directories, symlinks, etc.).
        // `file_type()` returns `None` for the root entry; skip those too.
        let ft = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if !ft.is_file() {
            // ft.is_symlink() → true for symlinks; ft.is_dir() → true for dirs.
            // Both are skipped here. Symlinks are never followed.
            continue;
        }

        let path = entry.path();

        // Apply hard exclusions by directory-component name.
        //
        // IMPORTANT: only inspect components of the path *relative to cwd*,
        // not of the absolute host path. Walking absolute ancestors can
        // wrongly exclude an entire checkout when a *parent* directory on the
        // host happens to be named "build", "target", etc.
        //
        // `rel_for_exclusion` is the sub-path below cwd. If strip_prefix
        // fails (should not happen for a walk rooted at cwd) we fall back to
        // only checking the final file-name component so we never silently
        // include paths we can't inspect.
        let rel_for_exclusion = path.strip_prefix(cwd).unwrap_or(path);
        let in_excluded_dir = rel_for_exclusion.components().any(|comp| {
            use std::path::Component;
            match comp {
                Component::Normal(name) => name
                    .to_str()
                    .map(|s| EXCLUDED_DIR_NAMES.contains(&s))
                    .unwrap_or(false),
                _ => false,
            }
        });
        if in_excluded_dir {
            continue;
        }

        // Skip by extension.
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if EXCLUDED_EXTS.contains(&ext) {
                continue;
            }
        }

        // Skip by size (100 MB hard cap).
        if let Ok(meta) = entry.metadata() {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        }

        out.push(path.to_path_buf());
    }

    out
}

/// Pseudo-UUID for call IDs (avoids pulling in the uuid crate here;
/// pi-sandbox already has uuid in its deps).
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize env-mutating tests to prevent race conditions.
    /// `std::env` is process-global; parallel tests that mutate it interfere.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Test 1: `E2bConfig::from_env` picks up `E2B_API_KEY` from the environment.
    ///
    /// Sets `E2B_API_KEY` to a known value, calls `E2bConfig::from_env()`, and
    /// asserts the key is present in the returned config.
    ///
    /// Note: `std::env` mutations are process-wide. Run with `--test-threads=1`
    /// if parallel env-mutating tests interfere.
    #[test]
    fn e2b_config_from_env_picks_up_api_key() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let prev_key = std::env::var("E2B_API_KEY").ok();
        let prev_offline = std::env::var("PI_SANDBOX_OFFLINE").ok();

        let expected_key = "test-e2b-from-env-key-12345";

        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("E2B_API_KEY", expected_key);
            std::env::remove_var("PI_SANDBOX_OFFLINE");
        }

        let result = E2bConfig::from_env();

        // Restore before asserting so we always clean up.
        #[allow(unused_unsafe)]
        unsafe {
            if let Some(v) = prev_key {
                std::env::set_var("E2B_API_KEY", v);
            } else {
                std::env::remove_var("E2B_API_KEY");
            }
            if let Some(v) = prev_offline {
                std::env::set_var("PI_SANDBOX_OFFLINE", v);
            }
        }

        let config = result.expect("from_env should succeed when E2B_API_KEY is set");
        assert_eq!(
            config.api_key, expected_key,
            "E2bConfig::from_env should read E2B_API_KEY"
        );
    }

    /// Test 2: `E2bConfig::from_env` returns an error when `E2B_API_KEY` is absent.
    ///
    /// Removes `E2B_API_KEY` from the environment, calls `E2bConfig::from_env()`,
    /// and asserts the specific `SandboxError::Unavailable` variant is returned.
    ///
    /// Note: `std::env` mutations are process-wide. Run with `--test-threads=1`
    /// if parallel env-mutating tests interfere.
    #[test]
    fn e2b_config_from_env_errors_when_no_key() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let prev_key = std::env::var("E2B_API_KEY").ok();
        let prev_offline = std::env::var("PI_SANDBOX_OFFLINE").ok();

        // Safety: unit tests running in a controlled env; we restore after.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::remove_var("E2B_API_KEY");
            std::env::remove_var("PI_SANDBOX_OFFLINE");
        }

        let err = E2bConfig::from_env()
            .expect_err("from_env should fail when E2B_API_KEY is absent");

        // Restore.
        #[allow(unused_unsafe)]
        unsafe {
            if let Some(v) = prev_key {
                std::env::set_var("E2B_API_KEY", v);
            }
            if let Some(v) = prev_offline {
                std::env::set_var("PI_SANDBOX_OFFLINE", v);
            }
        }

        assert!(
            matches!(err, SandboxError::Unavailable(_)),
            "expected Unavailable, got {:?}",
            err
        );
    }

    /// Test 3: `E2bProvider::with_key` does NOT make an HTTP call at construction.
    ///
    /// Strategy: bind a local TCP listener (so we can detect any incoming connection),
    /// point `E2B_BASE_URL` at it, call `with_key()`, then check that the listener
    /// received zero connections. A regression that makes the constructor connect
    /// would either succeed (connection visible in listener) or take non-trivial time.
    ///
    /// The test also verifies no session state is allocated before `execute_tool`.
    #[test]
    fn e2b_provider_construction_is_lazy() {
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;

        // Bind a local TCP listener on a random port. Any incoming connection
        // from `with_key()` would be accepted here.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local listener");
        listener
            .set_nonblocking(true)
            .expect("set listener non-blocking");
        let local_addr = listener.local_addr().expect("local_addr");
        let mock_url = format!("http://{}", local_addr);

        // Counter: any connection accepted by the listener increments this.
        let conn_count = StdArc::new(AtomicUsize::new(0));
        let conn_count_clone = conn_count.clone();

        // Spawn a quick checker thread that accepts any pending connections.
        let checker = std::thread::spawn(move || {
            // Accept any pending connections (non-blocking, so returns Err if none).
            while listener.accept().is_ok() {
                conn_count_clone.fetch_add(1, Ordering::SeqCst);
            }
        });

        // Save and override E2B_BASE_URL.
        let prev_url = std::env::var("E2B_BASE_URL").ok();
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("E2B_BASE_URL", &mock_url);
        }

        // Construct the provider — must NOT make any HTTP call.
        let provider = E2bProvider::with_key("dummy-key-construction-laziness".to_string());

        // Restore env.
        #[allow(unused_unsafe)]
        unsafe {
            if let Some(v) = prev_url {
                std::env::set_var("E2B_BASE_URL", v);
            } else {
                std::env::remove_var("E2B_BASE_URL");
            }
        }

        // Give the checker thread a moment to drain any pending connections.
        let _ = checker.join();

        // No connection should have been made during construction.
        let connections = conn_count.load(Ordering::SeqCst);
        assert_eq!(
            connections, 0,
            "E2bProvider::with_key made {connections} HTTP connection(s) during construction; expected 0"
        );

        // Also verify no session state is allocated.
        assert_eq!(provider.name(), "e2b");
        let state = provider.state.lock().unwrap();
        assert!(
            state.sandbox_id.is_none(),
            "sandbox_id should be None before first execute_tool"
        );
        assert!(!state.ready, "ready should be false before first execute_tool");
    }

    /// Test 4: `validate_flushback_path` rejects unsafe paths and accepts safe ones.
    ///
    /// Security regression test for the path-traversal fix described in RFD 0026
    /// §"Security boundary": a malformed or compromised remote response must not be
    /// able to overwrite host files outside the session cwd via absolute paths or
    /// `..` components.
    #[test]
    fn validate_flushback_path_rejects_unsafe_paths() {
        let cwd = std::path::Path::new("/home/user/project");

        // ── Accepted paths ────────────────────────────────────────────────────
        let ok_cases = [
            "src/main.rs",
            "Cargo.toml",
            "a/b/c/d.txt",
            "./src/lib.rs", // CurDir component is fine
            ".",
        ];
        for path in ok_cases {
            let result = E2bProvider::validate_flushback_path(cwd, path);
            assert!(
                result.is_ok(),
                "expected validate_flushback_path to accept '{}', got {:?}",
                path,
                result.err()
            );
            // The accepted path must be inside cwd.
            let joined = result.unwrap();
            assert!(
                joined.starts_with(cwd),
                "accepted path '{}' resolved to '{}' which is outside cwd '{}'",
                path,
                joined.display(),
                cwd.display()
            );
        }

        // ── Rejected paths ────────────────────────────────────────────────────
        let err_cases = [
            // Absolute paths
            "/etc/passwd",
            "/home/user/project/safe.txt", // absolute even though it's inside cwd
            // Parent-directory escape
            "../sibling/secret.txt",
            "src/../../etc/shadow",
            "a/b/../../../outside",
            // Double-dot at root
            "../../etc/hosts",
        ];
        for path in err_cases {
            let result = E2bProvider::validate_flushback_path(cwd, path);
            assert!(
                result.is_err(),
                "expected validate_flushback_path to reject '{}', but it returned Ok({:?})",
                path,
                result.ok().map(|p| p.display().to_string())
            );
        }
    }

    /// Test 5: `check_no_symlinks_in_parent_chain` rejects paths whose existing
    /// parent components are symlinks.
    ///
    /// A pre-existing symlink inside `cwd` (e.g. `link -> /outside`) passes the
    /// lexical `starts_with(cwd)` check in `validate_flushback_path` but would
    /// cause `write`/`rename` to follow it and overwrite files outside the session
    /// directory.  This test verifies the symlink guard catches that case.
    #[cfg(unix)]
    #[test]
    fn check_no_symlinks_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();

        // Create: cwd/safe_dir/  (real dir)
        let safe_dir = cwd.join("safe_dir");
        std::fs::create_dir(&safe_dir).unwrap();

        // A safe file inside a real dir — should be accepted.
        let safe_file = safe_dir.join("file.txt");
        assert!(
            E2bProvider::check_no_symlinks_in_parent_chain(cwd, &safe_file).is_ok(),
            "expected Ok for a path with no symlinks in parent chain"
        );

        // Create: cwd/link_dir -> /tmp  (symlink to directory outside cwd)
        let link_dir = cwd.join("link_dir");
        symlink("/tmp", &link_dir).expect("symlink creation");

        // A path inside the symlinked component — must be rejected.
        let via_link = link_dir.join("evil.txt");
        let result = E2bProvider::check_no_symlinks_in_parent_chain(cwd, &via_link);
        assert!(
            result.is_err(),
            "expected Err when a parent component is a symlink, got Ok"
        );
        let err_msg = result.unwrap_err();
        assert!(
            err_msg.contains("symlink"),
            "error message should mention 'symlink'; got: {err_msg}"
        );

        // Non-existent path (no symlinks in existing components) — should be accepted.
        let new_path = cwd.join("new_dir").join("new_file.txt");
        assert!(
            E2bProvider::check_no_symlinks_in_parent_chain(cwd, &new_path).is_ok(),
            "expected Ok for a non-existent path with no symlinks"
        );
    }
}
