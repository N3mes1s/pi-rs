//! RFD 0026 v2 Phase C2 — sandbox-side contextfs bootstrap for Sprites.
//!
//! Orchestrates everything that has to happen *inside* the sprite for the
//! contextfs RW `/work` mount to come up. Transport-agnostic via RFD 0029's
//! [`contextfs_mesh::ConnectionBlob`]: the caller exposes the host-side UDSes
//! with [`contextfs_mesh::expose_uds`] (or `_with_seed`), passes the two
//! blobs in, and the sandbox-side machinery materialises receive-uds peers
//! + contextfsd inside the sprite.
//!
//! Inputs (per [`SpriteBootstrap`]):
//!   - `&dyn wromm::provider::Provider` (drives file uploads + exec)
//!   - Host musl-static paths for `contextfsd` and `cfs-mesh`
//!   - 32-byte hex tenant secret
//!   - Cedar policy text
//!   - Two [`ConnectionBlob`]s — one for cfs-fs-server, one for the broker
//!
//! Side effects in the sprite (all idempotent within one call):
//!   1. Upload `contextfsd` and `cfs-mesh` to `/home/sprite/` (mode 0755).
//!   2. Privileged: `apt-get install -y fuse3` (Sprites is Ubuntu 24.04).
//!   3. Privileged: write `/etc/contextfs/tenant-secret` (0600).
//!   4. Privileged: write `/etc/contextfs/policy.cedar`.
//!   5. Privileged: write `/etc/contextfs/contextfsd.toml` targeting
//!      sprite-local UDS paths (`/tmp/cfs-fs-recv.sock`,
//!      `/tmp/broker-recv.sock`).
//!   6. Launch two `cfs-mesh receive-uds` peers in the background, one per
//!      blob, binding the local UDS paths.
//!   7. Wait for both local UDSes to appear.
//!   8. Launch `contextfsd` in the background, log to
//!      `/var/log/contextfsd.log`.
//!   9. Poll `mountpoint -q /work` until ready or `mount_timeout` expires.
//!
//! On any failure, the caller is expected to `try_destroy_sprite`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use contextfs_mesh::blob::ConnectionBlob;
use wromm::provider::Provider;

use crate::contextfs_embedder::{EmbedderTomlSpec, FuseAcl};

const CONTEXTFSD_SANDBOX_PATH: &str = "/home/sprite/contextfsd";
const CFS_MESH_SANDBOX_PATH: &str = "/home/sprite/cfs-mesh";

const TENANT_SECRET_SANDBOX_PATH: &str = "/etc/contextfs/tenant-secret";
const CEDAR_POLICY_SANDBOX_PATH: &str = "/etc/contextfs/policy.cedar";
const DAEMON_TOML_SANDBOX_PATH: &str = "/etc/contextfs/contextfsd.toml";
const DAEMON_LOG_SANDBOX_PATH: &str = "/var/log/contextfsd.log";

/// Sprite-local UDS the sandbox-side `cfs-mesh receive-uds` binds for the
/// cfs-fs-server tunnel. contextfsd's `[mount.remote_fs] target_uds` points
/// here.
const FS_RECV_UDS_SANDBOX: &str = "/tmp/cfs-fs-recv.sock";
/// Same for the broker.
const BROKER_RECV_UDS_SANDBOX: &str = "/tmp/broker-recv.sock";

const CACHE_DIR_SANDBOX_PATH: &str = "/var/cache/contextfs/work";
const SPRITE_MOUNTPOINT: &str = "/work";

const RECV_LOG_FS_SANDBOX: &str = "/var/log/cfs-mesh-recv-fs.log";
const RECV_LOG_BROKER_SANDBOX: &str = "/var/log/cfs-mesh-recv-broker.log";

/// All ways the bootstrap can fail.
#[derive(Debug)]
pub enum BootstrapError {
    /// `std::fs::read` of a host-side binary path failed.
    HostBinaryRead { path: PathBuf, source: std::io::Error },
    /// JSON serialisation of a ConnectionBlob failed (should never happen).
    BlobSerialize(serde_json::Error),
    /// `Provider::write_file_with_mode` failed in the sprite.
    SandboxWrite { step: &'static str, source: wromm::error::WrommError },
    /// `Provider::exec` or `exec_privileged` failed.
    SandboxExec { step: &'static str, detail: String },
    /// `Provider` returned an internal error.
    Provider { source: wromm::error::WrommError },
    /// A local UDS in the sprite never appeared (cfs-mesh receive-uds
    /// didn't bind in time).
    RecvUdsTimeout { uds: PathBuf, waited: Duration },
    /// `/work` never became a mountpoint inside the sprite within the deadline.
    MountTimeout { mountpoint: PathBuf, waited: Duration },
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootstrapError::HostBinaryRead { path, source } => {
                write!(f, "read host binary {}: {source}", path.display())
            }
            BootstrapError::BlobSerialize(e) => write!(f, "serialise ConnectionBlob: {e}"),
            BootstrapError::SandboxWrite { step, source } => {
                write!(f, "write_file_with_mode in step {step:?}: {source}")
            }
            BootstrapError::SandboxExec { step, detail } => {
                write!(f, "exec in step {step:?}: {detail}")
            }
            BootstrapError::Provider { source } => write!(f, "provider error: {source}"),
            BootstrapError::RecvUdsTimeout { uds, waited } => write!(
                f,
                "sandbox UDS {} never appeared within {:?}",
                uds.display(),
                waited
            ),
            BootstrapError::MountTimeout { mountpoint, waited } => write!(
                f,
                "{} never became a mountpoint within {:?}",
                mountpoint.display(),
                waited
            ),
        }
    }
}

impl std::error::Error for BootstrapError {}

/// Bootstrap inputs the caller assembles before invoking.
#[derive(Clone, Debug)]
pub struct SpriteBootstrap {
    /// Host path to the musl-static `contextfsd` binary.
    pub contextfsd_host_path: PathBuf,
    /// Host path to the musl-static `cfs-mesh` binary.
    pub cfs_mesh_host_path: PathBuf,
    /// 32 random bytes hex-encoded — lands at
    /// `/etc/contextfs/tenant-secret` (mode 0600).
    pub tenant_secret_hex: String,
    /// Cedar policy text the broker uses. Typically
    /// `microvm::broker_proxy::resolved_cedar_policy_text()`.
    pub cedar_policy_text: String,
    /// ConnectionBlob from [`contextfs_mesh::expose_uds`] applied to the
    /// host-side cfs-fs-server UDS. The sandbox-side receive-uds peer
    /// binds it at `/tmp/cfs-fs-recv.sock`.
    pub fs_blob: ConnectionBlob,
    /// Same but for the host-side contextfs-broker UDS. Sandbox-side
    /// binds at `/tmp/broker-recv.sock`.
    pub broker_blob: ConnectionBlob,
    /// How long to wait for `/tmp/cfs-fs-recv.sock` and
    /// `/tmp/broker-recv.sock` to materialise after launching the
    /// receive-uds peers. Recommend 8 s.
    pub recv_uds_timeout: Duration,
    /// How long to wait for `/work` to become a mountpoint after
    /// launching contextfsd. Recommend 10 s.
    pub mount_timeout: Duration,
}

/// Sprite-side state after `bootstrap_contextfs` returns Ok.
#[derive(Clone, Debug)]
pub struct SpriteBootstrapResult {
    /// `/var/log/contextfsd.log` inside the sprite — caller can tail it
    /// for diagnostics via `wromm exec`.
    pub daemon_log_sandbox_path: PathBuf,
    /// `/work` — always.
    pub mountpoint: PathBuf,
    /// Where the cfs-mesh receive-uds peers wrote their stderr.
    pub recv_fs_log_sandbox_path: PathBuf,
    pub recv_broker_log_sandbox_path: PathBuf,
}

/// Drive the bootstrap. Each step fails loudly with a [`BootstrapError`]
/// carrying the step name and underlying cause.
pub async fn bootstrap_contextfs(
    provider: &dyn Provider,
    sandbox_id: &str,
    plan: &SpriteBootstrap,
) -> Result<SpriteBootstrapResult, BootstrapError> {
    // ── 1. upload binaries ──
    upload_binary(
        provider,
        sandbox_id,
        &plan.contextfsd_host_path,
        CONTEXTFSD_SANDBOX_PATH,
    )
    .await?;
    upload_binary(
        provider,
        sandbox_id,
        &plan.cfs_mesh_host_path,
        CFS_MESH_SANDBOX_PATH,
    )
    .await?;

    // ── 2. fuse3 ──
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &["apt-get", "install", "-y", "fuse3"],
        "install_fuse3",
    )
    .await?;
    // Ensure user_allow_other so fuse_acl="all" works under a non-root daemon.
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &[
            "sh",
            "-c",
            "grep -q '^user_allow_other' /etc/fuse.conf 2>/dev/null \
             || echo user_allow_other >> /etc/fuse.conf",
        ],
        "ensure_user_allow_other",
    )
    .await?;

    // ── 3. tenant secret + policy + cache dir ──
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &["mkdir", "-p", "/etc/contextfs", CACHE_DIR_SANDBOX_PATH],
        "mkdir_dirs",
    )
    .await?;
    write_privileged(
        provider,
        sandbox_id,
        TENANT_SECRET_SANDBOX_PATH,
        plan.tenant_secret_hex.as_bytes(),
        0o600,
        "tenant_secret",
    )
    .await?;
    write_privileged(
        provider,
        sandbox_id,
        CEDAR_POLICY_SANDBOX_PATH,
        plan.cedar_policy_text.as_bytes(),
        0o644,
        "cedar_policy",
    )
    .await?;

    // ── 4. contextfsd.toml ──
    let toml = build_sprite_toml();
    write_privileged(
        provider,
        sandbox_id,
        DAEMON_TOML_SANDBOX_PATH,
        toml.as_bytes(),
        0o644,
        "daemon_toml",
    )
    .await?;

    // ── 5. launch cfs-mesh receive-uds (×2) ──
    let fs_blob_json =
        serde_json::to_string(&plan.fs_blob).map_err(BootstrapError::BlobSerialize)?;
    let broker_blob_json =
        serde_json::to_string(&plan.broker_blob).map_err(BootstrapError::BlobSerialize)?;
    launch_receive_uds(
        provider,
        sandbox_id,
        &fs_blob_json,
        FS_RECV_UDS_SANDBOX,
        RECV_LOG_FS_SANDBOX,
        "/var/run/cfs-mesh-recv-fs.pid",
        "launch_recv_fs",
    )
    .await?;
    launch_receive_uds(
        provider,
        sandbox_id,
        &broker_blob_json,
        BROKER_RECV_UDS_SANDBOX,
        RECV_LOG_BROKER_SANDBOX,
        "/var/run/cfs-mesh-recv-broker.pid",
        "launch_recv_broker",
    )
    .await?;

    // ── 6. wait for the recv UDSes ──
    wait_for_sandbox_path(provider, sandbox_id, FS_RECV_UDS_SANDBOX, plan.recv_uds_timeout)
        .await
        .map_err(|waited| BootstrapError::RecvUdsTimeout {
            uds: PathBuf::from(FS_RECV_UDS_SANDBOX),
            waited,
        })?;
    wait_for_sandbox_path(
        provider,
        sandbox_id,
        BROKER_RECV_UDS_SANDBOX,
        plan.recv_uds_timeout,
    )
    .await
    .map_err(|waited| BootstrapError::RecvUdsTimeout {
        uds: PathBuf::from(BROKER_RECV_UDS_SANDBOX),
        waited,
    })?;

    // ── 7. launch contextfsd ──
    let launch_cmd = format!(
        "nohup {bin} --config {toml} >{log} 2>&1 < /dev/null & echo $! >/var/run/contextfsd.pid",
        bin = CONTEXTFSD_SANDBOX_PATH,
        toml = DAEMON_TOML_SANDBOX_PATH,
        log = DAEMON_LOG_SANDBOX_PATH,
    );
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &["sh", "-c", &launch_cmd],
        "launch_contextfsd",
    )
    .await?;

    // ── 8. wait for /work to mount ──
    poll_mountpoint(provider, sandbox_id, plan.mount_timeout).await?;

    Ok(SpriteBootstrapResult {
        daemon_log_sandbox_path: PathBuf::from(DAEMON_LOG_SANDBOX_PATH),
        mountpoint: PathBuf::from(SPRITE_MOUNTPOINT),
        recv_fs_log_sandbox_path: PathBuf::from(RECV_LOG_FS_SANDBOX),
        recv_broker_log_sandbox_path: PathBuf::from(RECV_LOG_BROKER_SANDBOX),
    })
}

/// Render the canonical sprite-side TOML targeting the local
/// cfs-mesh receive-uds endpoints.
fn build_sprite_toml() -> String {
    let mut spec = EmbedderTomlSpec::sprite_default(PathBuf::from(FS_RECV_UDS_SANDBOX));
    spec.fuse_acl = FuseAcl::All;
    spec.broker_socket_path = PathBuf::from(BROKER_RECV_UDS_SANDBOX);
    spec.render()
}

async fn upload_binary(
    provider: &dyn Provider,
    sandbox_id: &str,
    host_src: &Path,
    sandbox_dst: &str,
) -> Result<(), BootstrapError> {
    let bytes = std::fs::read(host_src).map_err(|e| BootstrapError::HostBinaryRead {
        path: host_src.to_path_buf(),
        source: e,
    })?;
    provider
        .write_file_with_mode(sandbox_id, sandbox_dst, &bytes, 0o755)
        .await
        .map_err(|e| BootstrapError::SandboxWrite {
            step: "upload_binary",
            source: e,
        })?;
    Ok(())
}

async fn write_privileged(
    provider: &dyn Provider,
    sandbox_id: &str,
    dst: &str,
    bytes: &[u8],
    mode: u32,
    step: &'static str,
) -> Result<(), BootstrapError> {
    let chmod_octal = format!("{:o}", mode);
    let script = format!(
        "umask 077 && cat > {dst} && chmod {chmod_octal} {dst}",
        dst = shell_quote(dst),
        chmod_octal = chmod_octal,
    );
    let r = provider
        .exec_with_stdin(
            sandbox_id,
            &["sudo", "-n", "sh", "-c", &script],
            bytes,
        )
        .await
        .map_err(|e| BootstrapError::Provider { source: e })?;
    if r.exit_code != 0 {
        return Err(BootstrapError::SandboxExec {
            step,
            detail: format!(
                "exit={} stderr={:?}",
                r.exit_code,
                String::from_utf8_lossy(&r.stderr).chars().take(400).collect::<String>()
            ),
        });
    }
    Ok(())
}

async fn exec_privileged_or_err(
    provider: &dyn Provider,
    sandbox_id: &str,
    cmd: &[&str],
    step: &'static str,
) -> Result<(), BootstrapError> {
    let mut full: Vec<&str> = Vec::with_capacity(cmd.len() + 2);
    full.push("sudo");
    full.push("-n");
    full.extend_from_slice(cmd);
    let r = provider
        .exec(sandbox_id, &full)
        .await
        .map_err(|e| BootstrapError::Provider { source: e })?;
    if r.exit_code != 0 {
        return Err(BootstrapError::SandboxExec {
            step,
            detail: format!(
                "exit={} cmd={:?} stderr={:?}",
                r.exit_code,
                cmd,
                String::from_utf8_lossy(&r.stderr).chars().take(400).collect::<String>()
            ),
        });
    }
    Ok(())
}

/// Launch one `cfs-mesh receive-uds` peer in the background. Reads the
/// blob JSON from stdin via the wromm exec_with_stdin path so we don't
/// have to shell-quote it.
async fn launch_receive_uds(
    provider: &dyn Provider,
    sandbox_id: &str,
    blob_json: &str,
    bind_uds: &str,
    log_path: &str,
    pid_path: &str,
    step: &'static str,
) -> Result<(), BootstrapError> {
    // Persist the blob to a sprite-local temp file via `sudo tee`, then
    // launch receive-uds reading from it. Reasoning: passing a multi-KB
    // JSON blob via shell arg expansion is fragile (quoting, length);
    // passing via file is robust + leaves an audit artifact.
    let blob_file = format!("/run/cfs-mesh-blob-{}.json", uniq_suffix(bind_uds));
    let write_blob_script = format!(
        "umask 077 && cat > {blob_file} && chmod 0600 {blob_file}",
        blob_file = shell_quote(&blob_file)
    );
    let r = provider
        .exec_with_stdin(
            sandbox_id,
            &["sudo", "-n", "sh", "-c", &write_blob_script],
            blob_json.as_bytes(),
        )
        .await
        .map_err(|e| BootstrapError::Provider { source: e })?;
    if r.exit_code != 0 {
        return Err(BootstrapError::SandboxExec {
            step,
            detail: format!(
                "blob-write exit={} stderr={:?}",
                r.exit_code,
                String::from_utf8_lossy(&r.stderr).chars().take(200).collect::<String>()
            ),
        });
    }

    // Now launch receive-uds. cfs-mesh receive-uds blocks until SIGTERM;
    // we nohup it and capture pid for the teardown path. Export
    // CFS_MESH_BIN to the SAME binary path so that the receive_uds SDK
    // (which spawns a child `cfs-mesh agora-listen` for Agora transport)
    // can locate it inside the sprite where it is not on PATH.
    let launch = format!(
        "rm -f {uds}; \
         CFS_MESH_BIN={bin} nohup {bin} receive-uds --uds {uds} --blob @{blob_file} \
           >{log} 2>&1 < /dev/null & \
         echo $! > {pid}",
        bin = CFS_MESH_SANDBOX_PATH,
        uds = shell_quote(bind_uds),
        blob_file = shell_quote(&blob_file),
        log = shell_quote(log_path),
        pid = shell_quote(pid_path),
    );
    exec_privileged_or_err(provider, sandbox_id, &["sh", "-c", &launch], step).await?;
    Ok(())
}

/// Poll `test -S <path>` inside the sandbox until it returns 0 or we run
/// past `deadline`. Returns the elapsed time on timeout (so the caller
/// can wrap it in a more specific error variant naming the UDS).
async fn wait_for_sandbox_path(
    provider: &dyn Provider,
    sandbox_id: &str,
    sandbox_path: &str,
    deadline: Duration,
) -> Result<(), Duration> {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(150);
    loop {
        let r = provider
            .exec(sandbox_id, &["test", "-S", sandbox_path])
            .await;
        if matches!(r, Ok(ref r) if r.exit_code == 0) {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(start.elapsed());
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn poll_mountpoint(
    provider: &dyn Provider,
    sandbox_id: &str,
    deadline: Duration,
) -> Result<(), BootstrapError> {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(200);
    loop {
        let r = provider
            .exec(sandbox_id, &["mountpoint", "-q", SPRITE_MOUNTPOINT])
            .await
            .map_err(|e| BootstrapError::Provider { source: e })?;
        if r.exit_code == 0 {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(BootstrapError::MountTimeout {
                mountpoint: PathBuf::from(SPRITE_MOUNTPOINT),
                waited: start.elapsed(),
            });
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn uniq_suffix(s: &str) -> String {
    // Cheap stable derivative of `s` for unique file names. Used purely
    // to keep the two receive-uds blob files from colliding.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// Minimal POSIX shell-safe single-quote escape.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sprite_toml_targets_local_recv_udses() {
        let toml = build_sprite_toml();
        assert!(toml.contains("tenant_secret_path = \"/etc/contextfs/tenant-secret\""));
        assert!(toml.contains("mountpoint = \"/work\""));
        assert!(toml.contains("fuse_acl = \"all\""));
        assert!(toml.contains("caller_uid_passthrough = true"));
        assert!(toml.contains("auto_unmount = true"));
        assert!(toml.contains("read_only = false"));
        // The sprite-side daemon dials its receive-uds peers, not the
        // host UDSes directly.
        assert!(toml.contains(r#"target_uds = "/tmp/cfs-fs-recv.sock""#));
        assert!(toml.contains(r#"socket_path = "/tmp/broker-recv.sock""#));
        let _: toml::Value = toml::from_str(&toml).expect("rendered TOML parses");
    }

    #[test]
    fn shell_quote_round_trip() {
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("/etc/foo"), "'/etc/foo'");
        assert_eq!(shell_quote("with 'quote'"), "'with '\\''quote'\\'''");
    }

    #[test]
    fn uniq_suffix_is_stable_and_distinct() {
        assert_eq!(uniq_suffix("/tmp/a"), uniq_suffix("/tmp/a"));
        assert_ne!(uniq_suffix("/tmp/a"), uniq_suffix("/tmp/b"));
    }
}
