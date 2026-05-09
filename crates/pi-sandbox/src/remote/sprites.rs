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
//!
//! ## contextfs host-side daemons (PI_SPRITES_CONTEXTFS=1)
//!
//! When `PI_SPRITES_CONTEXTFS=1` is set, `ensure_session_open` additionally
//! spawns three host-side children and stores their room IDs in `SessionState`
//! for the M2 sprite-side bootstrap:
//!
//!   1. `cfs-fs-server --root <cwd> --socket <run_dir>/cfs-fs.sock`
//!   2. `contextfs-broker run --socket <run_dir>/broker.sock
//!                             --policy <run_dir>/cedar.policy
//!                             --tenant-secret-path <run_dir>/tenant.secret
//!                             --allowed-uid <self_uid>`
//!   3. `cfs-mesh agora-bridge --room <room_label_fs>
//!                             --target-uds <run_dir>/cfs-fs.sock`
//!      and a sibling for the broker UDS on a separate room label.
//!
//! Room labels are provisioned by shelling out to `agora create <label>`
//! (the agora binary is resolved via `PI_AGORA_BIN` → `which agora`).
//! The per-session `run_dir` lives under
//! `/home/nemesis/code/.pi-sprites/<sprite_label>/` so UDSes survive the
//! process lifetime (not tmpfs).
//!
//! Binary resolution env vars:
//!   `PI_CFS_MESH_BIN`                  — override path to `cfs-mesh`
//!   `PI_SANDBOX_CFS_FS_SERVER_BIN`     — override path to `cfs-fs-server`
//!   `PI_SANDBOX_CONTEXTFS_BROKER_BIN`  — override path to `contextfs-broker`
//!   `PI_AGORA_BIN`                     — override path to `agora`

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use pi_ai::AuthStorage;
use pi_tools::ToolContext;

use crate::provider::{SandboxError, SandboxExecution, SandboxProvider};

// ── contextfs host-side daemon helpers ──────────────────────────────────────
// Linux-only: contextfs uses Linux-specific FUSE + UDS infra.
// Gated on PI_SPRITES_CONTEXTFS=1 at runtime.

#[cfg(target_os = "linux")]
/// Resolve the `cfs-mesh` binary path.
///   1. `PI_CFS_MESH_BIN` env var (explicit override)
///   2. `which cfs-mesh` (PATH lookup)
/// Returns `None` if both fail; caller surfaces a clear error.
fn resolved_cfs_mesh() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PI_CFS_MESH_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("cfs-mesh").ok()
}

#[cfg(target_os = "linux")]
/// Resolve the `agora` binary path.
///   1. `PI_AGORA_BIN` env var (explicit override)
///   2. `which agora` (PATH lookup)
fn resolved_agora() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PI_AGORA_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("agora").ok()
}

#[cfg(target_os = "linux")]
/// Spawn `cfs-mesh agora-bridge --room <room_label> --target-uds <target>`
/// and return the live child handle. The caller holds it for the session's
/// lifetime; `kill_on_drop` ensures the bridge dies when the provider drops.
///
/// Binary resolved via `PI_CFS_MESH_BIN` env override → `which cfs-mesh`.
fn spawn_cfs_mesh_agora_bridge(
    room_label: &str,
    target_uds: &Path,
) -> Result<Child, SandboxError> {
    let bin = resolved_cfs_mesh().ok_or_else(|| {
        SandboxError::Unavailable(
            "cfs-mesh not found (set PI_CFS_MESH_BIN or put cfs-mesh on PATH;              needed by PI_SPRITES_CONTEXTFS=1;              build with `cd contextfs && cargo build --release --bin cfs-mesh`)"
                .into(),
        )
    })?;
    let child = Command::new(&bin)
        .arg("agora-bridge")
        .arg("--room")
        .arg(room_label)
        .arg("--target-uds")
        .arg(target_uds)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            SandboxError::Provider(format!(
                "spawn cfs-mesh agora-bridge ({}): {e}",
                bin.display()
            ))
        })?;
    debug!(
        bin = %bin.display(),
        room = %room_label,
        target = %target_uds.display(),
        "cfs-mesh agora-bridge spawned"
    );
    Ok(child)
}

#[cfg(target_os = "linux")]
/// Provision a fresh agora room by shelling out to `agora create <label>`.
/// Returns `(room_id, secret)` parsed from the CLI's stdout.
///
/// `agora create` prints lines like:
/// ```text
///   Room ID:    ag-xxxxxxxx
///   Secret:     <64 hex chars>
/// ```
///
/// If the `agora` binary is unavailable, returns `SandboxError::Unavailable`
/// with a helpful install hint.
async fn provision_agora_room(label: &str) -> Result<(String, String), SandboxError> {
    let bin = resolved_agora().ok_or_else(|| {
        SandboxError::Unavailable(
            "agora not found (set PI_AGORA_BIN or put agora on PATH;              needed by PI_SPRITES_CONTEXTFS=1 for room provisioning;              install from https://theagora.dev or build from source)"
                .into(),
        )
    })?;
    let out = Command::new(&bin)
        .arg("create")
        .arg(label)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            SandboxError::Provider(format!("spawn `agora create {label}`: {e}"))
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(SandboxError::Provider(format!(
            "`agora create {label}` failed (exit {}): {stderr}",
            out.status.code().unwrap_or(-1)
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let room_id = stdout
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("Room ID:")
                .map(|s| s.trim().to_string())
        })
        .ok_or_else(|| {
            SandboxError::Provider(format!(
                "`agora create {label}`: no 'Room ID:' in stdout:\n{stdout}"
            ))
        })?;
    let secret = stdout
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("Secret:")
                .map(|s| s.trim().to_string())
        })
        .ok_or_else(|| {
            SandboxError::Provider(format!(
                "`agora create {label}`: no 'Secret:' in stdout:\n{stdout}"
            ))
        })?;
    debug!(label, room_id = %room_id, "agora room provisioned");
    Ok((room_id, secret))
}

#[cfg(target_os = "linux")]
/// Per-session run directory, rooted under a well-known persistent location
/// so UDSes and secrets are NOT on tmpfs.
///
/// Default: `$PI_SPRITES_RUN_BASE/<label>/` where `PI_SPRITES_RUN_BASE`
/// defaults to `<this-process-binary-adjacent>/.pi-sprites` resolved at
/// runtime. Tests override via `PI_SPRITES_RUN_BASE`.
///
/// /tmp is explicitly avoided — on Linux dev hosts /tmp is often a tmpfs
/// mount with limited capacity and processes/files may not survive a reboot
/// or aggressive cleaning.
fn sprites_run_dir(label: &str) -> PathBuf {
    let base = if let Ok(p) = std::env::var("PI_SPRITES_RUN_BASE") {
        PathBuf::from(p)
    } else {
        // Fallback: ~/.pi/sprites-sessions/ — guaranteed persistent, writable
        // by the user, and not a tmpfs.
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pi")
            .join("sprites-sessions")
    };
    base.join(label)
}


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
struct SessionState {
    /// Sprite ID once `wromm up` has succeeded. None until first
    /// `execute_tool` call.
    sprite_id: Option<String>,
    /// True once the sprite is ready (worker uploaded + chmod'd).
    ready: bool,
    /// Set when host-side cleanup has poisoned the session.
    poisoned: Option<String>,
    session_open_start: Option<Instant>,
    /// Host-side daemon children (cfs-fs-server, contextfs-broker,
    /// cfs-mesh agora-bridge ×2). Populated only when
    /// `PI_SPRITES_CONTEXTFS=1`. These are `kill_on_drop`, so they
    /// die with the provider when `cleanup()` runs.
    host_children: Vec<Child>,
    /// Agora room label for the cfs-fs-server bridge (PI_SPRITES_CONTEXTFS=1).
    room_id_fs: String,
    /// Agora room label for the contextfs-broker bridge (PI_SPRITES_CONTEXTFS=1).
    room_id_broker: String,
    /// Per-session run directory: holds UDSes, cedar.policy, tenant.secret.
    /// Set during `ensure_session_open` when PI_SPRITES_CONTEXTFS=1.
    run_dir: Option<PathBuf>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            sprite_id: None,
            ready: false,
            poisoned: None,
            session_open_start: None,
            host_children: Vec::new(),
            room_id_fs: String::new(),
            room_id_broker: String::new(),
            run_dir: None,
        }
    }
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

        // 4. SmartSync the project tree into the sprite at /home/sprite/work.
        //    `wromm up` provisions the sandbox but does not sync arbitrary
        //    project files — only the wromm.json spec lands automatically.
        //    We mirror the E2B SmartSync model: walk the host cwd, filter
        //    out node_modules / target / large binaries, `wromm cp` each
        //    file into the sprite at /home/sprite/work/<rel-path>.
        if let Err(e) = self.smart_sync_to_sprite(&sprite_id, cwd).await {
            self.try_destroy_sprite(&sprite_id).await;
            return Err(e);
        }

        // ── contextfs host-side daemons (PI_SPRITES_CONTEXTFS=1) ────────
        // Linux-only: contextfs uses Linux-specific FUSE + UDS infra.
        //
        // IMPORTANT: any failure inside this block must call
        // `try_destroy_sprite` before returning, because the sprite was
        // already provisioned above (wromm up + worker upload + chmod all
        // succeeded). Without cleanup the sprite leaks until its own
        // timeout fires. We collect the result of an inner async block and
        // map errors through cleanup to enforce this invariant.
        #[cfg(target_os = "linux")]
        if std::env::var("PI_SPRITES_CONTEXTFS").as_deref() == Ok("1") {
            let contextfs_result: Result<(), SandboxError> = async {
                let run_dir = sprites_run_dir(&self.config.label);
                std::fs::create_dir_all(&run_dir).map_err(|e| {
                    SandboxError::Provider(format!(
                        "create sprites run_dir {}: {e}",
                        run_dir.display()
                    ))
                })?;

                let fs_sock = run_dir.join("cfs-fs.sock");
                let broker_sock = run_dir.join("broker.sock");
                let cedar_path = run_dir.join("cedar.policy");
                let secret_path = run_dir.join("tenant.secret");

                // Write cedar policy (full RW — broker is the policy gate).
                let policy_text =
                    crate::microvm::broker_proxy::resolved_cedar_policy_text();
                std::fs::write(&cedar_path, policy_text).map_err(|e| {
                    SandboxError::Provider(format!("write cedar.policy: {e}"))
                })?;

                // Write a random tenant secret (32 random bytes, hex-encoded).
                let tenant_secret: String = {
                    let raw: Vec<u8> = (0..32).map(|_| {
                        // cheap entropy via timestamp mixing
                        let t = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0u64, |d| d.as_nanos() as u64)
                            .wrapping_mul(0x9e3779b97f4a7c15u64)
                            .wrapping_add(rand_u64());
                        (t & 0xff) as u8
                    }).collect();
                    hex::encode(&raw)
                };
                std::fs::write(&secret_path, &tenant_secret).map_err(|e| {
                    SandboxError::Provider(format!("write tenant.secret: {e}"))
                })?;

                // 1. Spawn cfs-fs-server (RW — broker is the policy gate).
                let cfs_child =
                    crate::microvm::contextfs_proxy::spawn_cfs_fs_server(
                        cwd, &fs_sock, /*read_only=*/ false,
                    )
                    .await?;

                // 2. Spawn contextfs-broker.
                let broker_child =
                    crate::microvm::broker_proxy::spawn_contextfs_broker(
                        &broker_sock,
                        &cedar_path,
                        &secret_path,
                    )
                    .await?;

                // 3. Provision two agora rooms (one for fs-server, one for broker).
                let now_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0u64, |d| d.as_nanos() as u64);
                let fs_label = format!("pi-cfs-fs-{:x}", now_ns & 0xffff_ffff);
                let broker_label = format!("pi-cfs-br-{:x}", now_ns & 0xffff_ffff);

                let (room_id_fs, _fs_secret) =
                    provision_agora_room(&fs_label).await?;
                let (room_id_broker, _br_secret) =
                    provision_agora_room(&broker_label).await?;

                // 4. Spawn cfs-mesh agora-bridge for fs socket.
                let bridge_fs = spawn_cfs_mesh_agora_bridge(&fs_label, &fs_sock)?;

                // 5. Spawn cfs-mesh agora-bridge for broker socket.
                let bridge_broker =
                    spawn_cfs_mesh_agora_bridge(&broker_label, &broker_sock)?;

                // Store everything in session state.
                let mut st = self.state.lock().expect("state poisoned");
                st.host_children.push(cfs_child);
                st.host_children.push(broker_child);
                st.host_children.push(bridge_fs);
                st.host_children.push(bridge_broker);
                st.room_id_fs = room_id_fs;
                st.room_id_broker = room_id_broker;
                st.run_dir = Some(run_dir);
                Ok(())
            }
            .await;

            if let Err(e) = contextfs_result {
                // contextfs setup failed after the sprite was already
                // provisioned. Best-effort destroy the sprite so we don't
                // leak it, then surface the original error to the caller.
                // The caller's session state does not have `sprite_id` yet
                // (we return Err before `Ok(sprite_id)`) so cleanup() cannot
                // reach it — that's why we destroy it inline here.
                self.try_destroy_sprite(&sprite_id).await;
                return Err(e);
            }
        }

        Ok(sprite_id)
    }

    /// Walk the host cwd, filter out excluded paths (node_modules, target,
    /// large binaries) per .gitignore, and `wromm cp` each file into the
    /// sprite at `/home/sprite/work/<rel-path>`. Same SmartSync semantics
    /// as the E2B path.
    ///
    /// Sequential per-file cp keeps things simple at the wromm shell-out
    /// boundary; if upload throughput becomes a problem, batch files into
    /// a tar that we cp once and unpack inside the sprite.
    async fn smart_sync_to_sprite(
        &self,
        sprite_id: &str,
        cwd: &std::path::Path,
    ) -> Result<(), SandboxError> {
        let files = collect_sprites_upload_files(cwd);
        if files.is_empty() {
            // Still create /home/sprite/work so downstream exec calls have
            // a valid --work-dir even on an empty project.
            let mkdir = self
                .run_in_sprite(
                    sprite_id,
                    cwd,
                    "mkdir -p /home/sprite/work".into(),
                    15,
                    None,
                )
                .await?;
            if mkdir.exit_code != 0 {
                return Err(SandboxError::Provider(format!(
                    "mkdir /home/sprite/work in sprite failed: {}",
                    mkdir.stderr
                )));
            }
            return Ok(());
        }

        // Pre-create the directory tree inside the sprite in one shot so
        // wromm cp can land at known absolute paths.
        let mut dirs: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        dirs.insert("/home/sprite/work".to_string());
        for path in &files {
            if let Ok(rel) = path.strip_prefix(cwd) {
                if let Some(parent) = rel.parent() {
                    let posix: String = parent
                        .components()
                        .filter_map(|c| match c {
                            std::path::Component::Normal(n) => {
                                n.to_str().map(|s| s.to_owned())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    if !posix.is_empty() {
                        dirs.insert(format!("/home/sprite/work/{posix}"));
                    }
                }
            }
        }
        let mkdir_args = dirs
            .iter()
            .map(|d| format!("'{d}'"))
            .collect::<Vec<_>>()
            .join(" ");
        let mkdir = self
            .run_in_sprite(
                sprite_id,
                cwd,
                format!("mkdir -p {mkdir_args}"),
                30,
                None,
            )
            .await?;
        if mkdir.exit_code != 0 {
            return Err(SandboxError::Provider(format!(
                "mkdir project tree in sprite failed: {}",
                mkdir.stderr
            )));
        }

        // wromm cp each file. Sequential — wromm doesn't currently expose
        // a bulk-cp / tar-stream mode. For typical project sizes (a few
        // hundred files at <100 KB each) this is fine; for huge projects
        // we'd batch via tar.
        for path in files {
            let rel = path.strip_prefix(cwd).unwrap_or(&path);
            let posix_rel: String = rel
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(n) => {
                        n.to_str().map(|s| s.to_owned())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            let remote_path = format!("/home/sprite/work/{posix_rel}");
            let src = path.to_string_lossy().to_string();
            let cp_status = self
                .wromm_cmd()
                .current_dir(cwd)
                .arg("cp")
                .arg(format!("--id={sprite_id}"))
                .arg("--overwrite")
                .arg(&src)
                .arg(format!("sandbox:{remote_path}"))
                .stdout(Stdio::null())
                .status()
                .await
                .map_err(|e| {
                    SandboxError::Provider(format!(
                        "spawn `wromm cp {}`: {e}",
                        rel.display()
                    ))
                })?;
            if !cp_status.success() {
                return Err(SandboxError::Provider(format!(
                    "wromm cp {} failed (exit {})",
                    rel.display(),
                    cp_status.code().unwrap_or(-1)
                )));
            }
        }
        Ok(())
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
        // Worker runs in /home/sprite/work where smart_sync_to_sprite
        // synced the project tree. (When PI_SPRITES_CONTEXTFS=1 ships its
        // FUSE /work mount, that path will move to /work; for v1 we keep
        // the wromm-only project-sync path.)
        let cmd = format!(
            "{WORKER_REMOTE_PATH} --transport stdin --work-dir /home/sprite/work \
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

        // Flushback: apply file_writes to the host cwd. Mirrors the E2B
        // path. With contextfs RW /work this becomes a no-op (writes are
        // already live on host) but for the wromm-only sync path it's
        // essential — without it, write/edit tool changes never reach the
        // operator's filesystem.
        if !parsed.file_writes.is_empty() {
            apply_file_writes_to_host(&parsed.file_writes, cwd).await?;
        }

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

    /// Test-only entry point: open the host-side daemons without connecting
    /// to a real sprite. Used by integration tests that mock the child
    /// binaries and verify argv + process liveness.
    ///
    /// When `PI_SPRITES_CONTEXTFS=1`, this method:
    ///   - skips the `wromm` lifecycle (no wromm up / cp / chmod)
    ///   - runs only the contextfs host-side spawn path
    ///   - stores a synthetic `sprite_id` so the session looks "ready"
    ///
    /// Never call this in production code.
    ///
    /// Available under `#[cfg(test)]` (unit tests in this crate) AND
    /// under `#[cfg(feature = "test-helpers")]` (integration tests that
    /// depend on `pi-sandbox` with `features = ["test-helpers"]`).
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn _test_open_host_side_only(
        &self,
        cwd: &std::path::Path,
    ) -> Result<(), SandboxError> {
        // Check the required binary env vars before we try anything
        // (Linux-only check, since contextfs is Linux-only).
        #[cfg(target_os = "linux")]
        {
            let _cfs_fs = crate::microvm::contextfs_proxy::resolved_cfs_fs_server()
                .ok_or_else(|| {
                    SandboxError::Unavailable(
                        "cfs-fs-server not found (set PI_SANDBOX_CFS_FS_SERVER_BIN or                          put cfs-fs-server on PATH)"
                            .into(),
                    )
                })?;
        }

        // Run the same contextfs block as ensure_session_open but with a
        // synthetic sprite_id so we don't need wromm.
        {
            let mut st = self.state.lock().expect("state poisoned");
            st.sprite_id = Some("test-sprite-id".to_string());
        }

        #[cfg(target_os = "linux")]
        if std::env::var("PI_SPRITES_CONTEXTFS").as_deref() == Ok("1") {
            let run_dir = sprites_run_dir(&self.config.label);
            std::fs::create_dir_all(&run_dir).map_err(|e| {
                SandboxError::Provider(format!(
                    "create sprites run_dir {}: {e}",
                    run_dir.display()
                ))
            })?;

            let fs_sock = run_dir.join("cfs-fs.sock");
            let broker_sock = run_dir.join("broker.sock");
            let cedar_path = run_dir.join("cedar.policy");
            let secret_path = run_dir.join("tenant.secret");

            let policy_text =
                crate::microvm::broker_proxy::resolved_cedar_policy_text();
            std::fs::write(&cedar_path, policy_text).map_err(|e| {
                SandboxError::Provider(format!("write cedar.policy: {e}"))
            })?;
            std::fs::write(&secret_path, "test-secret-placeholder").map_err(|e| {
                SandboxError::Provider(format!("write tenant.secret: {e}"))
            })?;

            let cfs_child =
                crate::microvm::contextfs_proxy::spawn_cfs_fs_server(
                    cwd, &fs_sock, false,
                )
                .await?;

            let broker_child =
                crate::microvm::broker_proxy::spawn_contextfs_broker(
                    &broker_sock,
                    &cedar_path,
                    &secret_path,
                )
                .await?;

            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0u64, |d| d.as_nanos() as u64);
            let fs_label = format!("pi-cfs-fs-test-{:x}", now_ns & 0xffff_ffff);
            let broker_label = format!("pi-cfs-br-test-{:x}", now_ns & 0xffff_ffff);

            let (room_id_fs, _fs_secret) = provision_agora_room(&fs_label).await?;
            let (room_id_broker, _br_secret) =
                provision_agora_room(&broker_label).await?;

            let bridge_fs = spawn_cfs_mesh_agora_bridge(&fs_label, &fs_sock)?;
            let bridge_broker =
                spawn_cfs_mesh_agora_bridge(&broker_label, &broker_sock)?;

            let mut st = self.state.lock().expect("state poisoned");
            st.host_children.push(cfs_child);
            st.host_children.push(broker_child);
            st.host_children.push(bridge_fs);
            st.host_children.push(bridge_broker);
            st.room_id_fs = room_id_fs;
            st.room_id_broker = room_id_broker;
            st.run_dir = Some(run_dir);
            st.ready = true;
        }

        Ok(())
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
        let (id, children) = {
            let mut st = self.state.lock().expect("state poisoned");
            st.ready = false;
            let id = st.sprite_id.take();
            // Drain children — `kill_on_drop` fires in the Drop impl of each
            // `tokio::process::Child`, so draining the Vec is enough.
            let children = std::mem::take(&mut st.host_children);
            (id, children)
        };
        // Drop the children first so their kill_on_drop fires while we still
        // have async context. Then destroy the sprite.
        drop(children);
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
    /// File mutations (proto v2, RFD 0026). Empty if the worker
    /// produced a v1 response.
    #[serde(default)]
    file_writes: Vec<WorkerFileWrite>,
}

/// One file mutation reported by the worker. Path is relative to the
/// session cwd (the worker rejects absolute paths and `..`).
#[derive(Deserialize)]
struct WorkerFileWrite {
    path: String,
    contents_b64: String,
    #[allow(dead_code)]
    mode: u32,
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

/// Apply worker-supplied file writes to the host cwd (flushback).
///
/// Worker paths are relative to the session cwd. We validate before
/// touching the host filesystem:
///   - reject absolute paths
///   - reject any `..` component
///   - assert the joined path lexically lives inside cwd
///   - reject any pre-existing symlink in the parent chain (would let
///     a malicious payload escape the cwd via `link → /etc`)
///
/// Atomic write via temp file + rename, matching the E2B implementation.
async fn apply_file_writes_to_host(
    file_writes: &[WorkerFileWrite],
    cwd: &std::path::Path,
) -> Result<(), SandboxError> {
    use base64::Engine as _;
    for fw in file_writes {
        let host_path = validate_flushback_path_sprites(cwd, &fw.path)?;
        check_no_symlinks_in_parent_chain_sprites(cwd, &host_path)?;
        if let Some(parent) = host_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                SandboxError::Provider(format!(
                    "flushback create_dir_all '{}': {e}",
                    fw.path
                ))
            })?;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&fw.contents_b64)
            .map_err(|e| {
                SandboxError::Provider(format!(
                    "flushback base64 decode '{}': {e}",
                    fw.path
                ))
            })?;
        let tmp_path = {
            let mut t = host_path.clone();
            let stem = t
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            t.set_file_name(format!(".{stem}.__sprites_tmp__"));
            t
        };
        tokio::fs::write(&tmp_path, &bytes).await.map_err(|e| {
            SandboxError::Provider(format!(
                "flushback write '{}': {e}",
                fw.path
            ))
        })?;
        tokio::fs::rename(&tmp_path, &host_path).await.map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            SandboxError::Provider(format!(
                "flushback rename '{}': {e}",
                fw.path
            ))
        })?;
    }
    Ok(())
}

fn validate_flushback_path_sprites(
    cwd: &std::path::Path,
    raw: &str,
) -> Result<std::path::PathBuf, SandboxError> {
    use std::path::Component;
    let p = std::path::Path::new(raw);
    for component in p.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {
                return Err(SandboxError::Provider(format!(
                    "flushback path '{raw}' is absolute; only relative paths are permitted"
                )));
            }
            Component::ParentDir => {
                return Err(SandboxError::Provider(format!(
                    "flushback path '{raw}' contains '..'"
                )));
            }
            _ => {}
        }
    }
    let joined = cwd.join(p);
    if !joined.starts_with(cwd) {
        return Err(SandboxError::Provider(format!(
            "flushback path '{raw}' resolves outside cwd"
        )));
    }
    Ok(joined)
}

fn check_no_symlinks_in_parent_chain_sprites(
    cwd: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), SandboxError> {
    let mut p = target.to_path_buf();
    while p != *cwd {
        match std::fs::symlink_metadata(&p) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(SandboxError::Provider(format!(
                    "flushback parent chain contains symlink at '{}'",
                    p.display()
                )));
            }
            _ => {}
        }
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => break,
        }
    }
    Ok(())
}

/// Walk the host cwd and return the list of files to upload to the sprite.
/// Mirrors `e2b::collect_upload_files`'s exclusions + safety properties:
/// honors `.gitignore`, skips symlinks, drops generated dirs (node_modules,
/// target, .venv, …), drops files > 100 MB.
fn collect_sprites_upload_files(cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    use ignore::WalkBuilder;
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
    const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

    let mut walker = WalkBuilder::new(cwd);
    walker
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .standard_filters(true);
    let mut out = Vec::new();
    for entry in walker.build().flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        // Skip hard exclusion dirs.
        if path
            .components()
            .any(|c| match c {
                std::path::Component::Normal(n) => {
                    n.to_str().is_some_and(|s| EXCLUDED_DIR_NAMES.contains(&s))
                }
                _ => false,
            })
        {
            continue;
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if EXCLUDED_EXTS.contains(&ext) {
                continue;
            }
        }
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        }
        out.push(path.to_path_buf());
    }
    out
}
