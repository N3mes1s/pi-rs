//! Host-side glue for the contextfs `/work` mount in the guest
//! (RFD 0023 §3.5 / Commit G3 step 2).
//!
//! Two responsibilities:
//!
//! 1. **Spawn `cfs-fs-server`** as a subprocess, rooted at the
//!    VM's `host_cwd`, listening on a per-VM UDS at
//!    `<run_dir>/cfs-fs.sock`. We DO NOT reimplement the file
//!    server logic in pi-rs — `cfs-fs-server` is the contextfs
//!    binary that already knows how to serve a `LocalBackend`
//!    over its wire protocol.
//!
//! 2. **Bridge the vsock UDS to the cfs-fs-server UDS.** The
//!    guest's `contextfsd` `remote-fs` backend connects (via a
//!    sibling guest-side bridge) to vsock(2, 5005). Firecracker
//!    routes that to a host UNIX socket at
//!    `<vsock_path>_5005`. Pi-rs binds that UDS, accepts each
//!    connection, dials the cfs-fs-server UDS, and ferries
//!    bytes both directions until either side hangs up.
//!
//! Both halves are scoped to acquire→release of one VM. The
//! cfs-fs-server child is `kill_on_drop`, so it dies with the
//! VM. The bridge task aborts when the listener errors out.
//!
//! Located via `PI_SANDBOX_CFS_FS_SERVER_BIN` env var, falling
//! back to `which cfs-fs-server` on PATH. Fail-fast with a clear
//! error if the binary isn't available — the launcher returns
//! `SandboxError::Provider("…").

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tracing::{debug, warn};

use crate::provider::SandboxError;

/// Vsock port the host listens on for guest contextfs traffic.
/// Per RFD 0023 §"Wire protocol port assignments":
///   5001 — pi-sandbox-worker tool RPC (existing)
///   5003 — web_search proxy (existing, RFD §"web_search via vsock proxy")
///   5005 — contextfs remote-fs (this commit)
pub const VSOCK_CFS_PORT: u32 = 5005;

/// Resolve the `cfs-fs-server` binary path. Order:
///   1. `PI_SANDBOX_CFS_FS_SERVER_BIN` env var (explicit override)
///   2. `which cfs-fs-server` (PATH lookup)
/// Returns `None` if both fail; caller surfaces a clear error.
pub(crate) fn resolved_cfs_fs_server() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PI_SANDBOX_CFS_FS_SERVER_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("cfs-fs-server").ok()
}

/// Spawn `cfs-fs-server --root=<host_cwd> --socket=<sock_path>` and
/// return the live child handle. The caller holds the handle for
/// the VM's lifetime; `kill_on_drop` ensures the server dies with
/// the VM.
pub(crate) async fn spawn_cfs_fs_server(
    host_cwd: &Path,
    sock_path: &Path,
    read_only: bool,
) -> Result<Child, SandboxError> {
    let bin = resolved_cfs_fs_server().ok_or_else(|| {
        SandboxError::Provider(
            "cfs-fs-server not found (set PI_SANDBOX_CFS_FS_SERVER_BIN or put it on PATH; \
             build with `cd contextfs && cargo build --release --bin cfs-fs-server`)"
                .into(),
        )
    })?;
    if let Some(parent) = sock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(sock_path);
    let mut cmd = Command::new(&bin);
    cmd.arg("--root").arg(host_cwd).arg("--socket").arg(sock_path);
    // RW mode (PI_SANDBOX_CONTEXTFS_RW=1) drops --read-only so
    // cfs-fs-server accepts writes; the broker (Cedar PDP) becomes
    // the sole policy gate. Stacking cfs-fs-server --read-only on
    // top of an RW broker creates a confused failure mode where
    // the broker permits the write but cfs-fs-server returns
    // EROFS — the agent sees "broker said no" but it's actually
    // the data layer (per contextfs embedder-broker quickstart).
    if read_only {
        cmd.arg("--read-only");
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            SandboxError::Provider(format!(
                "spawn cfs-fs-server ({}): {e}",
                bin.display()
            ))
        })?;
    debug!(
        bin = %bin.display(),
        sock = %sock_path.display(),
        root = %host_cwd.display(),
        "cfs-fs-server spawned"
    );
    Ok(child)
}

/// Bind the per-VM contextfs vsock-side UDS at `<vsock_path>_5005`
/// and spawn a tokio task that forwards bytes to/from the
/// cfs-fs-server UDS at `target_uds`. Returns the bound UDS path
/// for cleanup tracking.
///
/// Called at cold-boot, before `wait_for_vsock_ready`. The UDS
/// must exist when the guest's bridge dials it; firecracker only
/// attempts the dial at the moment a guest process opens
/// vsock(2, 5005). Best-effort: if the bind fails, contextfs in
/// the guest will see "vsock: Connection refused" and the launcher
/// surfaces the warn.
pub(crate) fn spawn_cfs_vsock_bridge(
    vsock_path: &Path,
    target_uds: &Path,
) -> Result<PathBuf, std::io::Error> {
    // firecracker convention: <vsock_path>_<port>
    let mut p = vsock_path.as_os_str().to_owned();
    p.push(format!("_{VSOCK_CFS_PORT}"));
    let bridge_uds = PathBuf::from(p);
    let _ = std::fs::remove_file(&bridge_uds);
    let listener = UnixListener::bind(&bridge_uds)?;
    let target_uds = target_uds.to_path_buf();
    debug!(
        bridge = %bridge_uds.display(),
        target = %target_uds.display(),
        "contextfs vsock bridge bound"
    );

    let bridge_for_log = bridge_uds.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((from_guest, _)) => {
                    let target = target_uds.clone();
                    tokio::spawn(forward_one(from_guest, target));
                }
                Err(e) => {
                    debug!(
                        bridge = %bridge_for_log.display(),
                        err = %e,
                        "contextfs vsock bridge: accept failed; exiting"
                    );
                    break;
                }
            }
        }
    });

    Ok(bridge_uds)
}

async fn forward_one(from_guest: UnixStream, target_uds: PathBuf) {
    let to_server = match UnixStream::connect(&target_uds).await {
        Ok(s) => s,
        Err(e) => {
            warn!(
                target = %target_uds.display(),
                err = %e,
                "contextfs bridge: dial cfs-fs-server failed"
            );
            return;
        }
    };
    let (mut g_r, mut g_w) = from_guest.into_split();
    let (mut s_r, mut s_w) = to_server.into_split();
    let g_to_s = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut g_r, &mut s_w).await;
        let _ = s_w.shutdown().await;
    });
    let s_to_g = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut s_r, &mut g_w).await;
        let _ = g_w.shutdown().await;
    });
    let _ = g_to_s.await;
    let _ = s_to_g.await;
}
