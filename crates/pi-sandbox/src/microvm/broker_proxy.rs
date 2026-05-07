//! Host-side glue for the contextfs broker (Cedar verify_write
//! gate) — RFD 0023 §3.5 / Commit G3 step 3 (Cedar/RW phase).
//!
//! Twin of `contextfs_proxy` (which handles the cfs-fs-server +
//! `/work` file-ops bridge over vsock(2,5005)). This module owns
//! the policy-plane half:
//!
//! 1. **Spawn `contextfs-broker run --socket <broker_uds>`** as a
//!    subprocess, listening on a per-VM UDS at
//!    `<run_dir>/broker.sock`. The broker is operator-owned and
//!    lives as parent-of-pi-rs (RFD 0025 §D); for the v1 demo we
//!    spawn it kill_on_drop alongside each VM. Per-pi-process
//!    pooling is a future optimisation.
//!
//! 2. **Bridge the vsock UDS to the broker UDS.** The guest's
//!    contextfsd dials `/run/contextfs/broker.sock` (via a
//!    sibling guest-side bridge `pi-cfs-broker-vsock-bridge`) to
//!    vsock(2, 5006). Firecracker routes that to a host UNIX
//!    socket at `<vsock_path>_5006`. Pi-rs binds that UDS,
//!    accepts each connection, dials the broker UDS, and ferries
//!    bytes both directions until either side hangs up.
//!
//! Both halves are scoped to acquire→release of one VM. The
//! broker child is `kill_on_drop`, so it dies with the VM. The
//! bridge task aborts when the listener errors out.
//!
//! Located via `PI_SANDBOX_CONTEXTFS_BROKER_BIN` env var,
//! falling back to `which contextfs-broker` on PATH. Fail-fast
//! with a clear error if the binary isn't available — the
//! launcher returns `SandboxError::Provider("…")`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tracing::{debug, warn};

use crate::provider::SandboxError;

/// Vsock port the host listens on for guest contextfs broker
/// traffic. Per RFD 0023 §"Wire protocol port assignments":
///   5001 — pi-sandbox-worker tool RPC (existing)
///   5003 — web_search proxy (existing)
///   5005 — contextfs remote-fs (G3 step 2)
///   5006 — contextfs broker / Cedar verify_write (this commit)
pub const VSOCK_BROKER_PORT: u32 = 5006;

/// Resolve the `contextfs-broker` binary path. Order:
///   1. `PI_SANDBOX_CONTEXTFS_BROKER_BIN` env var (explicit override).
///   2. `which contextfs-broker` (PATH lookup).
/// Returns `None` if both fail; caller surfaces a clear error.
pub(crate) fn resolved_contextfs_broker() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PI_SANDBOX_CONTEXTFS_BROKER_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("contextfs-broker").ok()
}

/// Spawn `contextfs-broker run --socket <broker_uds> --policy
/// <cedar> --tenant-secret-path <secret> --allowed-uid 0` and
/// return the live child handle. The caller holds the handle for
/// the VM's lifetime; `kill_on_drop` ensures the broker dies with
/// the VM.
///
/// `cedar_policy_path` is the policy file the broker evaluates
/// every `Request::VerifyWrite` against. `tenant_secret_path`
/// must be the SAME file the in-guest contextfsd reads (per
/// contextfs RFD-0020 §"Decision-id determinism" — same secret
/// gives byte-identical local/remote decision ids; mismatched
/// secrets fail closed).
pub(crate) async fn spawn_contextfs_broker(
    broker_uds: &Path,
    cedar_policy_path: &Path,
    tenant_secret_path: &Path,
) -> Result<Child, SandboxError> {
    let bin = resolved_contextfs_broker().ok_or_else(|| {
        SandboxError::Provider(
            "contextfs-broker not found (set PI_SANDBOX_CONTEXTFS_BROKER_BIN or put it on PATH; \
             build with `cd contextfs && cargo build --release --bin contextfs-broker`)"
                .into(),
        )
    })?;
    if let Some(parent) = broker_uds.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(broker_uds);
    let child = Command::new(&bin)
        .arg("run")
        .arg("--socket")
        .arg(broker_uds)
        .arg("--policy")
        .arg(cedar_policy_path)
        .arg("--tenant-secret-path")
        .arg(tenant_secret_path)
        // Daemon (in-guest contextfsd) connects through the
        // host-side bridge proxy, which dials the broker UDS as
        // pi-rs's host UID. Only allow that UID.
        .arg("--allowed-uid")
        .arg(format!("{}", nix_uid_self()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            SandboxError::Provider(format!(
                "spawn contextfs-broker ({}): {e}",
                bin.display()
            ))
        })?;
    debug!(
        bin = %bin.display(),
        sock = %broker_uds.display(),
        policy = %cedar_policy_path.display(),
        "contextfs-broker spawned"
    );
    Ok(child)
}

/// Bind the per-VM broker vsock-side UDS at `<vsock_path>_5006`
/// and spawn a tokio task that forwards bytes to/from the broker
/// UDS at `target_uds`. Returns the bound UDS path for cleanup
/// tracking.
///
/// Mirrors `contextfs_proxy::spawn_cfs_vsock_bridge`. Best-effort:
/// if the bind fails, contextfsd's verify_write requests in the
/// guest hit "Connection refused" and the launcher logs a warn.
pub(crate) fn spawn_broker_vsock_bridge(
    vsock_path: &Path,
    target_uds: &Path,
) -> Result<PathBuf, std::io::Error> {
    let mut p = vsock_path.as_os_str().to_owned();
    p.push(format!("_{VSOCK_BROKER_PORT}"));
    let bridge_uds = PathBuf::from(p);
    let _ = std::fs::remove_file(&bridge_uds);
    let listener = UnixListener::bind(&bridge_uds)?;
    let target_uds = target_uds.to_path_buf();
    debug!(
        bridge = %bridge_uds.display(),
        target = %target_uds.display(),
        "contextfs-broker vsock bridge bound"
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
                        "contextfs-broker vsock bridge: accept failed; exiting"
                    );
                    break;
                }
            }
        }
    });

    Ok(bridge_uds)
}

async fn forward_one(from_guest: UnixStream, target_uds: PathBuf) {
    let to_broker = match UnixStream::connect(&target_uds).await {
        Ok(s) => s,
        Err(e) => {
            warn!(
                target = %target_uds.display(),
                err = %e,
                "contextfs-broker bridge: dial broker failed"
            );
            return;
        }
    };
    let (mut g_r, mut g_w) = from_guest.into_split();
    let (mut s_r, mut s_w) = to_broker.into_split();
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

fn nix_uid_self() -> u32 {
    // SAFETY: getuid is async-signal-safe and always succeeds.
    unsafe { libc::getuid() }
}

/// Cedar policy for the `tests_only` profile.
///
/// Permits read/list/stat/xattr.read unconditionally everywhere.
/// Permits write/create/delete/rename/commit only when the resource path
/// matches a `tests/` directory or a `*_test.rs` / `*_tests.rs` file.
///
/// This is the "give the agent a source tree to write tests against without
/// letting it touch the impl" pattern (RFD 0023 §"Profile selector").
///
/// **MUST stay byte-identical to the `tests_only` branch heredoc in
/// `crates/pi-sandbox-rootfs/build.sh`**. Both contextfsd (guest) and the
/// contextfs-broker (host) hash this text; if they differ even by a single
/// byte, contextfsd will refuse all write ops with a
/// `remote Cedar policy_hash skew` warning.
pub const TESTS_ONLY_CEDAR_POLICY: &str = r#"// pi-rs sandbox tests_only policy — read everywhere, write only inside
// tests/ directories or *_test.rs / *_tests.rs files.
// Agent::"pi-sandbox" principal. Unconditional read permits; conditional
// write/create/delete/rename/commit permits scoped to tests paths.
permit (
  principal,
  action == Action::"read",
  resource
);
permit (
  principal,
  action == Action::"list",
  resource
);
permit (
  principal,
  action == Action::"stat",
  resource
);
permit (
  principal,
  action == Action::"xattr.read",
  resource
);
permit (
  principal,
  action == Action::"write",
  resource
) when {
  resource.path like "*/tests/*" ||
  resource.path like "*/tests" ||
  resource.path like "*_test.rs" ||
  resource.path like "*_tests.rs"
};
permit (
  principal,
  action == Action::"create",
  resource
) when {
  resource.path like "*/tests/*" ||
  resource.path like "*/tests" ||
  resource.path like "*_test.rs" ||
  resource.path like "*_tests.rs"
};
permit (
  principal,
  action == Action::"delete",
  resource
) when {
  resource.path like "*/tests/*" ||
  resource.path like "*_test.rs" ||
  resource.path like "*_tests.rs"
};
permit (
  principal,
  action == Action::"rename",
  resource
) when {
  resource.path like "*/tests/*" ||
  resource.path like "*_test.rs" ||
  resource.path like "*_tests.rs"
};
permit (
  principal,
  action == Action::"commit",
  resource
) when {
  resource.path like "*/tests/*" ||
  resource.path like "*_test.rs" ||
  resource.path like "*_tests.rs"
};
"#;

/// Cedar profile selector for pi-rs sandbox VMs.
///
/// Both the host-side `contextfs-broker` and the in-guest `contextfsd`
/// daemon must hash byte-identical Cedar policy text. Profiles are
/// pre-baked constants so the exact text is available on both sides;
/// the selected profile's name is shipped on the kernel cmdline
/// (`pi.contextfs.cedar_profile=<token>`) and both sides pick the
/// matching text independently.
///
/// Select via `PI_SANDBOX_CEDAR_PROFILE` env var:
///   `tests_only` / `tests-only` → `CedarProfile::TestsOnly`
///   anything else (or unset)    → `CedarProfile::Default`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CedarProfile {
    /// Full read+write everywhere (the default open sandbox policy).
    Default,
    /// Read everywhere; write only inside `tests/` directories or
    /// `*_test.rs` / `*_tests.rs` files.
    TestsOnly,
}

impl CedarProfile {
    /// The token written to the kernel cmdline (no spaces, ASCII).
    pub(crate) fn cmdline_token(self) -> &'static str {
        match self {
            CedarProfile::Default => "default",
            CedarProfile::TestsOnly => "tests_only",
        }
    }

    /// The pre-baked Cedar policy text for this profile.
    pub(crate) fn policy_text(self) -> &'static str {
        match self {
            CedarProfile::Default => DEFAULT_CEDAR_POLICY,
            CedarProfile::TestsOnly => TESTS_ONLY_CEDAR_POLICY,
        }
    }
}

/// Read `PI_SANDBOX_CEDAR_PROFILE` and map to the matching [`CedarProfile`].
/// Accepts both `"tests_only"` and `"tests-only"` as the same variant.
/// Any other value (including unset) maps to [`CedarProfile::Default`].
pub(crate) fn resolved_cedar_profile() -> CedarProfile {
    match std::env::var("PI_SANDBOX_CEDAR_PROFILE")
        .ok()
        .as_deref()
    {
        Some("tests_only") | Some("tests-only") => CedarProfile::TestsOnly,
        _ => CedarProfile::Default,
    }
}

/// Default Cedar policy for the embedder demo.
///
/// Per contextfs's `docs/embedder-broker-quickstart.md`, prefer
/// explicit per-action permits over a single permit-all clause.
/// Two reasons (verbatim from contextfs):
///   (a) every audit allow-row shows which permit fired;
///       principle-of-least-privilege visible in pi-rs's tree.
///   (b) future contextfs releases that add new actions get
///       NoMatchingPermit instead of silently forwarding.
///
/// The principal entity id matches the
/// `default_principal = "Agent::\"pi-sandbox\""` line the
/// rootfs init writes into contextfsd.toml.
///
/// Override via `PI_SANDBOX_CEDAR_POLICY` env (path to a Cedar
/// file) or a host-launcher RuntimeConfig knob (future).
pub const DEFAULT_CEDAR_POLICY: &str = r#"// pi-rs sandbox demo policy — explicit per-action permits for
// Agent::"pi-sandbox". Anything not listed below NoMatchingPermit's
// (default-deny on contextfs's side). When contextfs adds new
// Action variants, this policy will fail closed for them until we
// extend the list — that is the design intent.
permit (
  principal,
  action == Action::"read",
  resource
);
permit (
  principal,
  action == Action::"list",
  resource
);
permit (
  principal,
  action == Action::"stat",
  resource
);
permit (
  principal,
  action == Action::"xattr.read",
  resource
);
permit (
  principal,
  action == Action::"write",
  resource
);
permit (
  principal,
  action == Action::"create",
  resource
);
permit (
  principal,
  action == Action::"delete",
  resource
);
permit (
  principal,
  action == Action::"rename",
  resource
);
permit (
  principal,
  action == Action::"commit",
  resource
);
"#;

/// Resolve the Cedar policy text the host writes into the per-VM
/// run dir. If `PI_SANDBOX_CEDAR_POLICY` points at a readable
/// file, use its contents; otherwise fall back to
/// `DEFAULT_CEDAR_POLICY`. Caller writes this to
/// `<run_dir>/policy.cedar` and passes the path to both the
/// broker (host-side) and the daemon's `[pdp].policy_path` (in
/// guest, via the rootfs init's TOML write).
pub(crate) fn resolved_cedar_policy_text() -> String {
    if let Ok(path) = std::env::var("PI_SANDBOX_CEDAR_POLICY") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            return text;
        }
    }
    resolved_cedar_profile().policy_text().to_string()
}
