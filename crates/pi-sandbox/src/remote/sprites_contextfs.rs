//! RFD 0026 v2 Phase C2 — sandbox-side contextfs bootstrap for Sprites.
//!
//! Orchestrates everything that has to happen *inside* the sprite for the
//! contextfs RW `/work` mount to come up. Transport-agnostic: the caller
//! provides the path to a sandbox-side UDS that already terminates the
//! host-tunnel (will be the path bound by `contextfs_mesh::receive_uds`
//! once RFD 0029's SDK lands).
//!
//! Inputs:
//!   - `&dyn wromm::provider::Provider` (drives file uploads + exec)
//!   - sprite-side paths for cfs-fs.sock + broker.sock (already tunneled)
//!   - host musl-static binary paths for `contextfsd` + `cfs-mesh`
//!   - cedar policy text + tenant secret (32 random bytes)
//!
//! Side effects in the sprite (all idempotent within one bootstrap call):
//!   1. Upload `contextfsd` and `cfs-mesh` to `/home/sprite/` (mode 0755).
//!   2. Privileged: `apt-get install -y fuse3` (Sprites is Ubuntu 24.04).
//!   3. Privileged: write `/etc/contextfs/tenant-secret` (mode 0600).
//!   4. Privileged: write `/etc/contextfs/policy.cedar`.
//!   5. Privileged: write `/etc/contextfs/contextfsd.toml`.
//!   6. Privileged: launch `contextfsd --config /etc/contextfs/contextfsd.toml`
//!      in the background, redirecting logs to `/var/log/contextfsd.log`.
//!   7. Poll `mountpoint /work` until it returns 0 or the deadline expires.
//!
//! On failure at any step, returns a `BootstrapError` with enough context
//! to drive a `try_destroy_sprite` cleanup at the caller.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wromm::provider::Provider;

use crate::contextfs_embedder::{EmbedderTomlSpec, FuseAcl};

/// Where binaries land inside the sprite. Sprite default user is `sprite`
/// (uid=1001) with `sudo` available; binaries live under `/home/sprite/`
/// so the unprivileged user can `chmod +x` them at upload time.
const CONTEXTFSD_SANDBOX_PATH: &str = "/home/sprite/contextfsd";
const CFS_MESH_SANDBOX_PATH: &str = "/home/sprite/cfs-mesh";

const TENANT_SECRET_SANDBOX_PATH: &str = "/etc/contextfs/tenant-secret";
const CEDAR_POLICY_SANDBOX_PATH: &str = "/etc/contextfs/policy.cedar";
const DAEMON_TOML_SANDBOX_PATH: &str = "/etc/contextfs/contextfsd.toml";
const DAEMON_LOG_SANDBOX_PATH: &str = "/var/log/contextfsd.log";

const CACHE_DIR_SANDBOX_PATH: &str = "/var/cache/contextfs/work";
const SPRITE_MOUNTPOINT: &str = "/work";

/// All ways the bootstrap can fail. Each variant carries the sprite-side
/// step name + provider error for the caller's audit log.
#[derive(Debug)]
pub enum BootstrapError {
    /// `std::fs::read` of a host-side binary path failed.
    HostBinaryRead { path: PathBuf, source: std::io::Error },
    /// `Provider::write_file_with_mode` failed in the sprite.
    SandboxWrite { step: &'static str, source: wromm::error::WrommError },
    /// `Provider::exec` or `exec_privileged` failed (non-zero exit OR I/O).
    SandboxExec { step: &'static str, detail: String },
    /// `Provider` returned an internal error.
    Provider { source: wromm::error::WrommError },
    /// `/work` never became a mountpoint inside the sprite within the deadline.
    MountTimeout { mountpoint: PathBuf, waited: Duration },
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootstrapError::HostBinaryRead { path, source } => {
                write!(f, "read host binary {}: {source}", path.display())
            }
            BootstrapError::SandboxWrite { step, source } => {
                write!(f, "write_file_with_mode in step {step:?}: {source}")
            }
            BootstrapError::SandboxExec { step, detail } => {
                write!(f, "exec in step {step:?}: {detail}")
            }
            BootstrapError::Provider { source } => write!(f, "provider error: {source}"),
            BootstrapError::MountTimeout { mountpoint, waited } => {
                write!(
                    f,
                    "{} never became a mountpoint within {:?}",
                    mountpoint.display(),
                    waited
                )
            }
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
    /// 32 random bytes hex-encoded — exact text that will land in
    /// `/etc/contextfs/tenant-secret` (mode 0600).
    pub tenant_secret_hex: String,
    /// Cedar policy text the broker will use. Caller provides; usually
    /// the value of `microvm::broker_proxy::resolved_cedar_policy_text()`.
    pub cedar_policy_text: String,
    /// Sprite-side UDS path bound by `contextfs_mesh::receive_uds` for
    /// the cfs-fs-server tunnel. The TOML's `[mount.remote_fs] target_uds`.
    pub fs_target_uds_sandbox: PathBuf,
    /// Maximum time to wait for `/work` to become a mountpoint after
    /// launching contextfsd. Recommend 10 s; the daemon's mount-syscall
    /// path is bounded.
    pub mount_timeout: Duration,
}

/// Sprite-side state after `bootstrap_contextfs` returns Ok. The caller
/// can use this for follow-up `exec`s or to render diagnostics.
#[derive(Clone, Debug)]
pub struct SpriteBootstrapResult {
    /// Where contextfsd's stderr/stdout is being written inside the sprite.
    pub daemon_log_sandbox_path: PathBuf,
    /// Where `/work` is mounted (always `SPRITE_MOUNTPOINT` today).
    pub mountpoint: PathBuf,
}

/// Drive the seven-step bootstrap. Each step that touches the sprite
/// fails loudly with a `BootstrapError` carrying the step name and the
/// underlying cause. Caller wraps in `try_destroy_sprite` on Err.
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

    // ── 2. apt-get install -y fuse3 ──
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &["apt-get", "install", "-y", "fuse3"],
        "install_fuse3",
    )
    .await?;

    // ── 3. tenant secret ──
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &["mkdir", "-p", "/etc/contextfs"],
        "mkdir_etc_contextfs",
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

    // ── 4. cedar policy ──
    write_privileged(
        provider,
        sandbox_id,
        CEDAR_POLICY_SANDBOX_PATH,
        plan.cedar_policy_text.as_bytes(),
        0o644,
        "cedar_policy",
    )
    .await?;

    // ── 5. contextfsd.toml ──
    let toml = build_sprite_toml(&plan.fs_target_uds_sandbox);
    write_privileged(
        provider,
        sandbox_id,
        DAEMON_TOML_SANDBOX_PATH,
        toml.as_bytes(),
        0o644,
        "daemon_toml",
    )
    .await?;
    exec_privileged_or_err(
        provider,
        sandbox_id,
        &["mkdir", "-p", CACHE_DIR_SANDBOX_PATH],
        "mkdir_cache_dir",
    )
    .await?;

    // ── 6. launch contextfsd in the background ──
    // sh -c so we get `>` redirection + `&` backgrounding through one exec.
    // `nohup` so the daemon survives `wromm exec` returning. `disown` would
    // be ideal but `nohup` + `&` is the portable form.
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

    // ── 7. wait for /work to become a mountpoint ──
    poll_mountpoint(provider, sandbox_id, plan.mount_timeout).await?;

    Ok(SpriteBootstrapResult {
        daemon_log_sandbox_path: PathBuf::from(DAEMON_LOG_SANDBOX_PATH),
        mountpoint: PathBuf::from(SPRITE_MOUNTPOINT),
    })
}

/// Render the canonical sprite-side TOML against a given fs target UDS.
/// Always uses the production embedder profile (`fuse_acl = "all"`,
/// `read_only = false`, sprite-default paths).
fn build_sprite_toml(fs_target_uds: &Path) -> String {
    let mut spec = EmbedderTomlSpec::sprite_default(fs_target_uds.to_path_buf());
    // Pin fuse_acl explicitly for grep-friendliness.
    spec.fuse_acl = FuseAcl::All;
    spec.render()
}

/// `wromm cp` analogue via Provider::write_file_with_mode.
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

/// Write a file inside the sprite with `mode`, using `sudo tee` so it can
/// land at root-owned paths under `/etc/`. Provider::write_file is
/// not-privileged on Sprites (drops to the `sprite` user), so we go via
/// exec_privileged for these.
async fn write_privileged(
    provider: &dyn Provider,
    sandbox_id: &str,
    dst: &str,
    bytes: &[u8],
    mode: u32,
    step: &'static str,
) -> Result<(), BootstrapError> {
    // sh -c 'umask 077; cat > <dst>' under sudo, fed `bytes` on stdin.
    // umask + chmod after pins the desired mode.
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

/// Run a command under `sudo -n` and fail loudly on non-zero exit.
async fn exec_privileged_or_err(
    provider: &dyn Provider,
    sandbox_id: &str,
    cmd: &[&str],
    step: &'static str,
) -> Result<(), BootstrapError> {
    // Prepend `sudo -n` so password-less sudo is used; fail if not avail.
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

/// Poll `mountpoint /work` until it returns 0 or `deadline` expires.
/// Caller picks the deadline (`SpriteBootstrap.mount_timeout`).
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

/// Minimal POSIX shell-safe single-quote escape — good enough for
/// path arguments built from constants. Strings come from
/// hard-coded `const &str`s today; this is belt-and-braces in case
/// callers ever pass user input.
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
    fn build_sprite_toml_uses_production_paths_and_fuse_acl_all() {
        let toml = build_sprite_toml(Path::new("/tmp/sprite-fs.sock"));
        assert!(toml.contains("tenant_secret_path = \"/etc/contextfs/tenant-secret\""));
        assert!(toml.contains("mountpoint = \"/work\""));
        assert!(toml.contains("fuse_acl = \"all\""));
        assert!(toml.contains("caller_uid_passthrough = true"));
        assert!(toml.contains("auto_unmount = true"));
        assert!(toml.contains("read_only = false"));
        assert!(toml.contains("target_uds = \"/tmp/sprite-fs.sock\""));
        // Round-trip parse.
        let _: toml::Value = toml::from_str(&toml).expect("rendered TOML must parse");
    }

    #[test]
    fn shell_quote_round_trip() {
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("/etc/foo"), "'/etc/foo'");
        assert_eq!(shell_quote("with 'quote'"), "'with '\\''quote'\\'''");
    }
}
