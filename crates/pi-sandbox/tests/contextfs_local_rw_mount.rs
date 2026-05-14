//! Local contextfs RW /work mount round-trip — no microVM, no remote
//! sandbox, no transport in between. Validates the inner stack (cfs-fs-server
//! + contextfs-broker + contextfsd) on the host with the canonical embedder
//! TOML.
//!
//! Why: pi-sandbox already has `firecracker_contextfs_rw_mount` (microVM
//! path) and the Sprites path is being built. Both share the same inner
//! stack; this test pins its behavior in isolation so any future change
//! that breaks the inner contract surfaces here, fast, without needing
//! firecracker or a Sprites credential.
//!
//! Topology under test:
//!
//!   <src_dir>  ──LocalBackend──▶  cfs-fs-server  ──UDS──▶  contextfsd
//!                                                                 │
//!                                                                 ▼ FUSE
//!                                                              <mnt_dir>
//!
//!   contextfs-broker ──UDS──▶ contextfsd  (Cedar verify_write gate)
//!
//! Gate: PI_SANDBOX_CONTEXTFS_LOCAL=1 + binaries on PATH (or env overrides).
//! Skipped cleanly when binaries are missing — same shape as
//! `firecracker_contextfs_rw_mount`.
//!
//! Embedder TOML knobs exercised (matching the project memory):
//!   caller_uid_passthrough = true
//!   fuse_acl               = "auto"   (local non-root daemon → Owner ACL;
//!                                      production Sprite uses "all" with
//!                                      user_allow_other in /etc/fuse.conf)
//!   auto_unmount           = true
//!   read_only              = false

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

fn skip(reason: &str) -> bool {
    eprintln!("SKIP: contextfs_local_rw_mount: {reason}");
    true
}

/// Resolve a binary path: explicit env var override → PATH → None.
fn resolve_bin(env_var: &str, bin_name: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which(bin_name).ok()
}

fn require_env(name: &str) -> bool {
    std::env::var(name).is_ok()
}

async fn wait_for_socket(p: &Path, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if p.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn wait_for_mountpoint(p: &Path, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if is_mountpoint(p) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Cheap "is this a mountpoint?" check. The kernel exposes
/// `/proc/self/mountinfo` with every mount listed by path.
fn is_mountpoint(p: &Path) -> bool {
    let Ok(s) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let target = p.to_string_lossy();
    s.lines().any(|line| {
        // mountinfo column 5 is the mount target.
        line.split_whitespace().nth(4) == Some(target.as_ref())
    })
}

const CEDAR_POLICY: &str = r#"
permit (principal, action == Action::"read",       resource);
permit (principal, action == Action::"list",       resource);
permit (principal, action == Action::"stat",       resource);
permit (principal, action == Action::"xattr.read", resource);
permit (principal, action == Action::"write",      resource);
permit (principal, action == Action::"create",     resource);
permit (principal, action == Action::"delete",     resource);
permit (principal, action == Action::"rename",     resource);
permit (principal, action == Action::"commit",     resource);
"#;

/// Build the canonical embedder contextfsd.toml as a String. Same shape
/// the Sprites contextfs orchestration will produce; factor into a helper
/// in `pi_sandbox` once the API stabilises.
fn embedder_toml(
    tenant_secret: &Path,
    audit_log: &Path,
    cedar: &Path,
    broker_sock: &Path,
    mount_name: &str,
    mountpoint: &Path,
    cache_dir: &Path,
    fs_target_uds: &Path,
    fuse_acl: &str,
) -> String {
    format!(
        r#"tenant_secret_path = {tenant_secret:?}
audit_log_path = {audit_log:?}

[pdp]
policy_path = {cedar:?}
default_principal = 'Agent::"pi-spike"'

[broker]
socket_path = {broker_sock:?}

[[mount]]
name = {mount_name:?}
mountpoint = {mountpoint:?}
backend = "remote-fs"
cache_dir = {cache_dir:?}
caller_uid_passthrough = true
fuse_acl = {fuse_acl:?}
auto_unmount = true
read_only = false

[mount.remote_fs]
target_uds = {fs_target_uds:?}
"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_contextfs_rw_mount_round_trip() {
    if !require_env("PI_SANDBOX_CONTEXTFS_LOCAL") {
        skip("PI_SANDBOX_CONTEXTFS_LOCAL=1 not set");
        return;
    }
    let Some(cfs_fs_server) = resolve_bin("PI_SANDBOX_CFS_FS_SERVER_BIN", "cfs-fs-server") else {
        skip("cfs-fs-server not found");
        return;
    };
    let Some(broker_bin) = resolve_bin("PI_SANDBOX_CONTEXTFS_BROKER_BIN", "contextfs-broker") else {
        skip("contextfs-broker not found");
        return;
    };
    let Some(contextfsd_bin) = resolve_bin("PI_SANDBOX_CONTEXTFSD_BIN", "contextfsd") else {
        skip("contextfsd not found");
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let src = root.join("src");
    let mnt = root.join("mnt");
    let run = root.join("run");
    let cache = run.join("cache");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&mnt).unwrap();
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(&cache).unwrap();

    // Seed source dir.
    std::fs::write(src.join("host-A.txt"), b"host-written-1").unwrap();
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/x.txt"), b"in-sub").unwrap();

    // Tenant secret (32 random bytes hex, mode 0600).
    let tenant_secret = run.join("tenant.secret");
    let raw: [u8; 32] = rand_bytes_32();
    std::fs::write(&tenant_secret, hex_encode(&raw)).unwrap();
    std::fs::set_permissions(&tenant_secret, std::fs::Permissions::from_mode(0o600)).unwrap();

    let cedar = run.join("policy.cedar");
    std::fs::write(&cedar, CEDAR_POLICY).unwrap();

    let fs_sock = run.join("cfs-fs.sock");
    let broker_sock = run.join("broker.sock");
    let audit_log = run.join("audit.ndjson");
    let daemon_toml = run.join("contextfsd.toml");
    std::fs::write(
        &daemon_toml,
        embedder_toml(
            &tenant_secret,
            &audit_log,
            &cedar,
            &broker_sock,
            "work",
            &mnt,
            &cache,
            &fs_sock,
            // local non-root daemon → SessionACL::Owner under "auto"; mounting
            // UID is the only allowed accessor, which is what this test wants.
            "auto",
        ),
    )
    .unwrap();

    // ── start cfs-fs-server ──
    let _ = std::fs::remove_file(&fs_sock);
    let _fs_child = spawn_logging(
        Command::new(&cfs_fs_server)
            .arg("--root")
            .arg(&src)
            .arg("--socket")
            .arg(&fs_sock)
            .arg("--allowed-uid")
            .arg(format!("{}", nix_uid_self()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true),
    );

    // ── start contextfs-broker ──
    let _ = std::fs::remove_file(&broker_sock);
    let _broker_child = spawn_logging(
        Command::new(&broker_bin)
            .arg("run")
            .arg("--socket")
            .arg(&broker_sock)
            .arg("--policy")
            .arg(&cedar)
            .arg("--tenant-secret-path")
            .arg(&tenant_secret)
            .arg("--allowed-uid")
            .arg(format!("{}", nix_uid_self()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true),
    );

    assert!(
        wait_for_socket(&fs_sock, Duration::from_secs(5)).await,
        "cfs-fs-server socket never appeared"
    );
    assert!(
        wait_for_socket(&broker_sock, Duration::from_secs(5)).await,
        "broker socket never appeared"
    );

    // ── start contextfsd ──
    let _daemon_child = spawn_logging(
        Command::new(&contextfsd_bin)
            .arg("--config")
            .arg(&daemon_toml)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true),
    );

    assert!(
        wait_for_mountpoint(&mnt, Duration::from_secs(10)).await,
        "FUSE mount at {} never appeared", mnt.display()
    );

    // ── invariants ──
    // T1: host→mount read
    let m_a = std::fs::read(mnt.join("host-A.txt")).expect("read host-A.txt");
    assert_eq!(m_a, b"host-written-1", "T1: contents differ");

    // T1b: subdir traversal
    let m_sx = std::fs::read(mnt.join("sub/x.txt")).expect("read sub/x.txt");
    assert_eq!(m_sx, b"in-sub", "T1b: subdir differs");

    // T2: mount→host write
    let payload = format!("mount-written-{}", now_nanos());
    std::fs::write(mnt.join("from-mount.txt"), &payload).unwrap();
    let host_seen = std::fs::read_to_string(src.join("from-mount.txt")).expect("host sees");
    assert_eq!(host_seen, payload, "T2: write not propagated to host");

    // T3: host edit visible in mount
    let edited = format!("host-edit-{}", now_nanos());
    std::fs::write(src.join("host-A.txt"), &edited).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let from_mount = std::fs::read_to_string(mnt.join("host-A.txt")).expect("read host-A");
    assert_eq!(from_mount, edited, "T3: host edit not reflected");

    // T4: FUSE create
    std::fs::write(mnt.join("created-via-fuse.txt"), b"line1\nline2\n").unwrap();
    let host_content = std::fs::read(src.join("created-via-fuse.txt")).expect("host sees create");
    assert_eq!(host_content, b"line1\nline2\n", "T4: create round-trip");

    // T5: FUSE delete
    std::fs::remove_file(mnt.join("created-via-fuse.txt")).unwrap();
    assert!(
        !src.join("created-via-fuse.txt").exists(),
        "T5: delete didn't propagate"
    );

    // T6: FUSE rename
    std::fs::write(mnt.join("rename-src.txt"), b"renaming-me").unwrap();
    std::fs::rename(mnt.join("rename-src.txt"), mnt.join("rename-dst.txt")).unwrap();
    assert!(src.join("rename-dst.txt").exists(), "T6: rename dest missing");
    assert!(
        !src.join("rename-src.txt").exists(),
        "T6: rename src not removed"
    );

    // T7: 1 MiB binary round-trip
    let mut rng_bytes = vec![0u8; 1024 * 1024];
    fill_random(&mut rng_bytes);
    std::fs::write(mnt.join("big.bin"), &rng_bytes).unwrap();
    let host_big = std::fs::read(src.join("big.bin")).expect("host sees big.bin");
    assert_eq!(host_big, rng_bytes, "T7: 1 MiB content diverged");
}

/// Spawn a child and drain its stderr in the background so a verbose daemon
/// can't backpressure us. Returns the handle (kill_on_drop fires on the
/// owning Vec/binding going out of scope at test end).
fn spawn_logging(cmd: &mut Command) -> Child {
    let mut child = cmd.spawn().expect("spawn child");
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
    }
    child
}

fn nix_uid_self() -> u32 {
    // SAFETY: getuid is always safe; returns the current real UID.
    unsafe { libc::getuid() }
}

fn rand_bytes_32() -> [u8; 32] {
    let mut out = [0u8; 32];
    fill_random(&mut out);
    out
}

fn fill_random(buf: &mut [u8]) {
    // Cheap PRNG seeded from clock — fine for a non-crypto fixture.
    let mut state = now_nanos() ^ 0x9e3779b97f4a7c15u64;
    for b in buf.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (state >> 56) as u8;
    }
}

fn hex_encode(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() * 2);
    for &x in b {
        out.push_str(&format!("{:02x}", x));
    }
    out
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
