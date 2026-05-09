//! Sprites remote sandbox provider — RFD 0026 v2.
//!
//! Sprites are persistent, auto-hibernating Firecracker microVMs on Fly.io,
//! served by https://api.sprites.dev. Where E2B's base template enforces
//! `PR_SET_NO_NEW_PRIVS=1` and a seccomp filter that block FUSE mounts
//! entirely, Sprites runs Ubuntu 24.04 with `NoNewPrivs=0`, no seccomp
//! filter installed, and `sudo` available — so `contextfs RW /work` mounts
//! cleanly inside a sprite. (See RFD 0026 §6/§7 for the E2B blocker probe
//! and §"v2-draft" for the architecture.)
//!
//! ## Architecture (linking three crates)
//!
//! - **wromm** — sandbox lifecycle. We shell out to `wromm up`, `wromm cp`,
//!   `wromm exec`, `wromm rm` rather than re-implement Sprites' HTTP API.
//!   wromm already has a battle-tested Sprites client at
//!   `wromm/src/providers/sprites.rs` plus uniform handling for vendor
//!   quirks (timeouts, hibernation, identity bootstrap).
//! - **contextfs** — `/work` mount. cfs-fs-server runs on the host serving
//!   the user's project dir over UDS; cfs-mesh agora-bridge wraps that UDS
//!   in an agora room; inside the sprite, cfs-mesh agora-listen exposes a
//!   local UDS that contextfsd FUSE-mounts at `/work` via its `remote-fs`
//!   backend. Bidirectional, real-time, no SmartSync upload + flushback
//!   hacks.
//! - **agora** — encrypted relay for the contextfs UDS bytestream. Both
//!   peers (host + sprite) only need outbound network.
//!
//! pi-rs's `SpritesProvider` is the orchestrator: it owns the host-side
//! daemon children, drives the sprite lifecycle, and dispatches tool
//! calls through `wromm exec` against the running sprite.
//!
//! ## v1 (this commit) — wromm lifecycle only
//!
//! This commit lands the lifecycle plumbing: `from_auth()`, sprite
//! provision via `wromm up --provider=sprites`, worker upload via
//! `wromm cp`, tool dispatch via `wromm exec`, `wromm rm` on `cleanup()`.
//! /work in v1 is whatever wromm sets up by default (project file sync at
//! `wromm up`); the contextfs+agora mount layer ships in the follow-up
//! commit, gated on `PI_SPRITES_CONTEXTFS=1` until dogfooded.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use pi_ai::AuthStorage;
use pi_tools::ToolContext;

use crate::provider::{SandboxError, SandboxExecution, SandboxProvider};

/// Default provider name — lower-cased for the telemetry slug.
const PROVIDER_NAME: &str = "sprites";

/// Where the worker lives inside the sprite. Under `/home/sprite/` so the
/// unprivileged sprite user (uid=1001) can chmod +x.
const WORKER_REMOTE_PATH: &str = "/home/sprite/pi-sandbox-worker";

/// Configuration for [`SpritesProvider`].
#[derive(Clone, Debug)]
pub struct SpritesConfig {
    /// Sprites API token. Required (set via `SPRITES_TOKEN` env var or
    /// `cfg.auth_storage.get("sprites")`).
    pub token: String,
    /// Path to the `wromm` binary. Defaults to `wromm` on PATH; override
    /// via `PI_WROMM_BIN` env var.
    pub wromm_bin: PathBuf,
    /// Sandbox label. Determines the sprite name on Sprites' side. Default
    /// `pi-rs-<short-uuid>`.
    pub label: String,
    /// Per-call exec timeout in seconds. Default 600.
    pub exec_timeout_secs: u32,
}

impl SpritesConfig {
    /// Resolve `wromm` from `PI_WROMM_BIN` env var or PATH lookup.
    fn resolve_wromm_bin() -> PathBuf {
        if let Ok(p) = std::env::var("PI_WROMM_BIN") {
            return PathBuf::from(p);
        }
        which::which("wromm").unwrap_or_else(|_| PathBuf::from("wromm"))
    }

    fn default_label() -> String {
        // Short pseudo-UUID — enough entropy to avoid label collisions
        // across concurrent pi sessions on the same Sprites account.
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("pi-rs-{:x}", now_ns & 0xffff_ffff)
    }
}

/// Per-session state held inside the lifecycle mutex.
#[derive(Default)]
struct SessionState {
    /// Sprite ID once `wromm up` has succeeded. None until first
    /// `execute_tool` call.
    sprite_id: Option<String>,
    /// True once the sprite is ready (worker uploaded + chmod'd).
    ready: bool,
    /// Set when host-side cleanup has poisoned the session.
    poisoned: Option<String>,
    session_open_start: Option<Instant>,
}

/// Sprites SandboxProvider. v1 dispatches tool calls via `wromm exec`.
pub struct SpritesProvider {
    config: SpritesConfig,
    state: Arc<Mutex<SessionState>>,
    open_lock: Arc<AsyncMutex<()>>,
}

impl SpritesProvider {
    /// Build from explicit token (testing escape hatch).
    pub fn with_token(token: String) -> Self {
        Self::new(SpritesConfig {
            token,
            wromm_bin: SpritesConfig::resolve_wromm_bin(),
            label: SpritesConfig::default_label(),
            exec_timeout_secs: 600,
        })
    }

    /// Build from environment + auth storage. Resolution order:
    /// 1. `SPRITES_TOKEN` env var
    /// 2. `auth_storage.get("sprites")` (RFD 0027 §H5 secure auth path)
    pub fn from_auth(auth: &AuthStorage) -> Result<Self, SandboxError> {
        let token = if let Ok(tok) = std::env::var("SPRITES_TOKEN") {
            if tok.trim().is_empty() {
                return Err(SandboxError::Unavailable(
                    "SPRITES_TOKEN env var is empty".into(),
                ));
            }
            tok
        } else if let Some(method) = auth.get("sprites") {
            match method {
                pi_ai::AuthMethod::ApiKey { value } if !value.trim().is_empty() => value,
                _ => {
                    return Err(SandboxError::Unavailable(
                        "auth storage has `sprites` but it isn't a usable ApiKey".into(),
                    ));
                }
            }
        } else {
            return Err(SandboxError::Unavailable(
                "SPRITES_TOKEN not set and no `sprites` entry in auth storage".into(),
            ));
        };
        Ok(Self::new(SpritesConfig {
            token,
            wromm_bin: SpritesConfig::resolve_wromm_bin(),
            label: SpritesConfig::default_label(),
            exec_timeout_secs: std::env::var("PI_SPRITES_EXEC_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(600),
        }))
    }

    fn new(config: SpritesConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(SessionState::default())),
            open_lock: Arc::new(AsyncMutex::new(())),
        }
    }

    /// Build a `wromm` Command with `SPRITES_TOKEN` injected into the
    /// child env (so the user doesn't need it in their global env).
    fn wromm_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.config.wromm_bin);
        cmd.env("SPRITES_TOKEN", &self.config.token);
        // Force JSON output for any subcommand we parse.
        cmd
    }

    /// Provision the sprite, returning its ID. Idempotent across calls
    /// (skips work if `state.ready`).
    async fn ensure_session_open(
        &self,
        cwd: &std::path::Path,
    ) -> Result<String, SandboxError> {
        // Worker bin must exist on the host before we try to upload it.
        let worker_bin = std::env::var("PI_SANDBOX_WORKER_BIN").map_err(|_| {
            SandboxError::Unavailable(
                "PI_SANDBOX_WORKER_BIN not set; pi-sandbox-worker must be \
                 built (musl-static) and its path set via this env var"
                    .into(),
            )
        })?;
        if !std::path::Path::new(&worker_bin).exists() {
            return Err(SandboxError::Unavailable(format!(
                "PI_SANDBOX_WORKER_BIN={worker_bin} does not exist"
            )));
        }

        // wromm needs a `wromm.json` in cwd. Generate a minimal one if
        // the user hasn't set one up — pi-rs's contract is "the cwd is
        // the project dir", same as every other provider.
        let spec_path = cwd.join("wromm.json");
        if !spec_path.exists() {
            let minimal = r#"{
  "name": "pi-rs-session",
  "runtimes": [],
  "system_packages": [],
  "services": [],
  "ports": [],
  "env": {},
  "source": {"type": "Manual"},
  "agent": null
}
"#;
            tokio::fs::write(&spec_path, minimal).await.map_err(|e| {
                SandboxError::Provider(format!("write minimal wromm.json: {e}"))
            })?;
        }

        // 1. wromm up --provider=sprites --label=<label> --wait --json
        let label = &self.config.label;
        let mut cmd = self.wromm_cmd();
        cmd.current_dir(cwd)
            .arg("up")
            .arg("--provider=sprites")
            .arg(format!("--label={label}"))
            .arg("--wait")
            .arg("--non-interactive")
            .arg("--json");
        let out = cmd.output().await.map_err(|e| {
            SandboxError::Provider(format!("spawn `wromm up`: {e}"))
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SandboxError::Provider(format!(
                "wromm up failed (exit {}): {stderr}",
                out.status.code().unwrap_or(-1)
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let last_json_line = stdout
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{'))
            .ok_or_else(|| {
                SandboxError::Provider(format!(
                    "wromm up: no JSON object in stdout: {stdout}"
                ))
            })?;
        let resp: WrommUpResponse = serde_json::from_str(last_json_line)
            .map_err(|e| {
                SandboxError::Provider(format!(
                    "wromm up: parse JSON: {e}: {last_json_line}"
                ))
            })?;
        let sprite_id = resp.sandbox_id;
        debug!(sprite_id, "sprites: provisioned");

        // 2. wromm cp <local-worker> sandbox:<remote>
        let cp_status = self
            .wromm_cmd()
            .current_dir(cwd)
            .arg("cp")
            .arg(format!("--id={sprite_id}"))
            .arg("--overwrite")
            .arg(&worker_bin)
            .arg(format!("sandbox:{WORKER_REMOTE_PATH}"))
            .stdout(Stdio::null())
            .status()
            .await
            .map_err(|e| {
                SandboxError::Provider(format!("spawn `wromm cp`: {e}"))
            })?;
        if !cp_status.success() {
            self.try_destroy_sprite(&sprite_id).await;
            return Err(SandboxError::Provider(format!(
                "wromm cp worker failed (exit {})",
                cp_status.code().unwrap_or(-1)
            )));
        }

        // 3. chmod +x the worker via wromm exec.
        let chmod = self
            .run_in_sprite(
                &sprite_id,
                cwd,
                format!("chmod +x {WORKER_REMOTE_PATH}"),
                30,
                None,
            )
            .await?;
        if chmod.exit_code != 0 {
            self.try_destroy_sprite(&sprite_id).await;
            return Err(SandboxError::Provider(format!(
                "chmod worker exited with code {}: {}",
                chmod.exit_code, chmod.stderr
            )));
        }

        Ok(sprite_id)
    }

    /// Run a shell command inside the sprite via `wromm exec`. Returns the
    /// captured stdout, stderr, and exit code. Optional stdin is passed
    /// via the child's stdin pipe (used by `dispatch_tool` to feed the
    /// worker its `ToolRequest` JSON line).
    async fn run_in_sprite(
        &self,
        sprite_id: &str,
        cwd: &std::path::Path,
        cmd: String,
        timeout_secs: u32,
        stdin: Option<String>,
    ) -> Result<ExecResult, SandboxError> {
        let mut wcmd = self.wromm_cmd();
        wcmd.current_dir(cwd)
            .arg("exec")
            .arg(format!("--id={sprite_id}"))
            .arg(format!("--timeout={timeout_secs}"))
            .arg("--json")
            .arg("--")
            .arg("bash")
            .arg("-c")
            .arg(cmd);
        if stdin.is_some() {
            wcmd.stdin(Stdio::piped());
        } else {
            wcmd.stdin(Stdio::null());
        }
        wcmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = wcmd.spawn().map_err(|e| {
            SandboxError::Provider(format!("spawn `wromm exec`: {e}"))
        })?;
        if let (Some(input), Some(mut sin)) = (stdin, child.stdin.take()) {
            sin.write_all(input.as_bytes()).await.map_err(|e| {
                SandboxError::Provider(format!("write stdin to wromm exec: {e}"))
            })?;
            // Drop sin to close — without this, the worker blocks on read.
            drop(sin);
        }
        let out = child.wait_with_output().await.map_err(|e| {
            SandboxError::Provider(format!("wait on `wromm exec`: {e}"))
        })?;
        // wromm --json prints exactly one JSON object with exit_code,
        // stdout, stderr fields (per wromm exec --help).
        let stdout = String::from_utf8_lossy(&out.stdout);
        let json_line = stdout
            .lines()
            .rev()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or("");
        if json_line.is_empty() {
            return Err(SandboxError::Provider(format!(
                "wromm exec: no JSON in stdout. exit={}, stderr={}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let resp: WrommExecResponse =
            serde_json::from_str(json_line).map_err(|e| {
                SandboxError::Provider(format!(
                    "wromm exec: parse JSON: {e}: {json_line}"
                ))
            })?;
        Ok(ExecResult {
            exit_code: resp.exit_code,
            stdout: resp.stdout.unwrap_or_default(),
            stderr: resp.stderr.unwrap_or_default(),
        })
    }

    /// Best-effort destroy. Logs warnings on failure rather than
    /// propagating, since cleanup runs on the unhappy path.
    async fn try_destroy_sprite(&self, sprite_id: &str) {
        let res = self
            .wromm_cmd()
            .arg("rm")
            .arg(sprite_id)
            .arg("--force")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .await;
        match res {
            Ok(s) if s.success() => {
                debug!(sprite_id, "sprite destroyed");
            }
            Ok(s) => warn!(
                sprite_id,
                code = s.code().unwrap_or(-1),
                "wromm rm: non-zero exit"
            ),
            Err(e) => warn!(sprite_id, %e, "spawn `wromm rm` failed"),
        }
    }

    /// Drive one tool call through the worker running inside the sprite.
    async fn dispatch_tool(
        &self,
        sprite_id: &str,
        cwd: &std::path::Path,
        tool_name: &str,
        tool_input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<SandboxExecution, SandboxError> {
        use pi_sandbox_protocol::{ToolRequest, CURRENT_PROTOCOL_VERSION};

        let tool_timeout_ms: u32 = tool_input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(120_000)
            .min(600_000) as u32;

        let request = ToolRequest {
            proto_version: CURRENT_PROTOCOL_VERSION,
            call_id: format!("sprites-{:x}", rand_u64()),
            tool_name: tool_name.to_string(),
            tool_input: tool_input.clone(),
            max_output_bytes: ctx.max_output_bytes as u32,
            timeout_ms: tool_timeout_ms,
        };
        let req_json = serde_json::to_string(&request)
            .map_err(|e| SandboxError::Provider(e.to_string()))?
            + "\n";

        // The worker reads ToolRequest on stdin, writes ToolResponse to
        // stdout. We pipe the JSON through `wromm exec`'s stdin and read
        // the result back from its captured stdout.
        let cmd = format!(
            "{WORKER_REMOTE_PATH} --transport stdin --work-dir /home/sprite \
             --log-level warn"
        );
        let exec = self
            .run_in_sprite(
                sprite_id,
                cwd,
                cmd,
                (tool_timeout_ms / 1000) + 30,
                Some(req_json),
            )
            .await?;

        // Parse the ToolResponse from the worker's stdout.
        let parsed: WorkerToolResponse =
            serde_json::from_str(exec.stdout.trim()).map_err(|e| {
                SandboxError::Provider(format!(
                    "worker exited with code {}; ToolResponse parse failed: {e}; \
                     stdout snippet: {snip}; worker stderr: {}",
                    exec.exit_code,
                    exec.stderr,
                    snip = exec.stdout.chars().take(400).collect::<String>(),
                ))
            })?;
        Ok(SandboxExecution {
            stdout: parsed.stdout,
            stderr: parsed.stderr,
            exit_status: if parsed.is_error && parsed.exit_status == 0 {
                1
            } else {
                parsed.exit_status
            },
            round_trip_ms: None, // filled by caller
            cost_usd: None,
        })
    }
}

#[async_trait]
impl SandboxProvider for SpritesProvider {
    fn name(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<SandboxExecution, SandboxError> {
        // Poison check.
        {
            let st = self.state.lock().expect("SpritesProvider state poisoned");
            if let Some(msg) = &st.poisoned {
                return Err(SandboxError::Provider(msg.clone()));
            }
        }

        let call_start = Instant::now();

        // Single-flight session open.
        let sprite_id = {
            let _guard = self.open_lock.lock().await;
            let already_ready = {
                let st = self.state.lock().expect("state poisoned");
                st.ready
            };
            if !already_ready {
                {
                    let mut st = self.state.lock().expect("state poisoned");
                    if st.session_open_start.is_none() {
                        st.session_open_start = Some(call_start);
                    }
                }
                let id = self.ensure_session_open(&ctx.cwd).await?;
                {
                    let mut st = self.state.lock().expect("state poisoned");
                    st.sprite_id = Some(id.clone());
                    st.ready = true;
                }
                id
            } else {
                let st = self.state.lock().expect("state poisoned");
                st.sprite_id.clone().expect("ready but no sprite_id")
            }
        };

        let mut exec = self
            .dispatch_tool(&sprite_id, &ctx.cwd, tool_name, tool_input, ctx)
            .await?;
        let elapsed_ms = call_start.elapsed().as_millis();
        exec.round_trip_ms = Some(elapsed_ms.min(u32::MAX as u128) as u32);
        // Sprites pricing isn't unit-priced like E2B's per-second compute;
        // wromm wraps Fly.io's machine pricing internally. Leave cost_usd
        // unset for v1 — sprites users see usage on their Sprites dashboard.
        Ok(exec)
    }

    async fn cleanup(&self) -> Result<(), SandboxError> {
        let id = {
            let mut st = self.state.lock().expect("state poisoned");
            st.ready = false;
            st.sprite_id.take()
        };
        if let Some(id) = id {
            self.try_destroy_sprite(&id).await;
        }
        Ok(())
    }
}

// ── wire types ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WrommUpResponse {
    sandbox_id: String,
}

#[derive(Deserialize)]
struct WrommExecResponse {
    exit_code: i32,
    #[serde(default)]
    stdout: Option<String>,
    #[serde(default)]
    stderr: Option<String>,
}

/// ToolResponse shape from the worker's stdout. Mirrors
/// `pi_sandbox_protocol::ToolResponse` with `file_writes` as default-empty
/// for proto-v1 compatibility.
#[derive(Deserialize)]
struct WorkerToolResponse {
    stdout: String,
    stderr: String,
    exit_status: i32,
    is_error: bool,
}

struct ExecResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Cheap pseudo-random u64 for call_id labelling. Quality is irrelevant —
/// just collision avoidance within a session.
fn rand_u64() -> u64 {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Mix in addr-of-stack for a touch of entropy.
    let stack: u64 = &now as *const _ as usize as u64;
    now.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(stack)
}
