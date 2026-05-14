//! contextfs RW /work mount end-to-end using RFD 0029's `Transport::Loopback`
//! variant — exercises the cfs-mesh SDK call shape (`expose_uds` +
//! `receive_uds`) without needing an agora relay.
//!
//! Distinct from `contextfs_local_rw_mount.rs`: that test wires contextfsd
//! straight to the cfs-fs-server UDS. This one inserts the SDK's two-call
//! exposure path (host: `expose_uds(host_uds, Loopback)`; sandbox:
//! `receive_uds(blob, local_uds)`) and points contextfsd at the local UDS.
//! For `Loopback` the receive side is a symlink to the host path — but the
//! call shape and `ConnectionBlob` serde round-trip are still exercised,
//! which catches regressions in the SDK contract that pi-rs consumes.
//!
//! Gate: PI_SANDBOX_CONTEXTFS_LOCAL=1 + cfs-fs-server / contextfs-broker /
//! contextfsd on PATH (or env overrides). Skipped cleanly otherwise.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use contextfs_mesh::blob::Transport;
use contextfs_mesh::{expose_uds, receive_uds};
use pi_sandbox::contextfs_embedder::{EmbedderTomlSpec, FuseAcl};

fn skip(reason: &str) {
    eprintln!("SKIP: contextfs_loopback_rw_mount: {reason}");
}

fn resolve_bin(env_var: &str, bin_name: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which(bin_name).ok()
}

fn nix_uid_self() -> u32 {
    unsafe { libc::getuid() }
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

fn is_mountpoint(p: &Path) -> bool {
    let Ok(s) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let target = p.to_string_lossy();
    s.lines()
        .any(|line| line.split_whitespace().nth(4) == Some(target.as_ref()))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_contextfs_rw_mount_via_sdk_calls() {
    if std::env::var("PI_SANDBOX_CONTEXTFS_LOCAL").is_err() {
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
    let recv = run.join("recv");
    for d in [&src, &mnt, &run, &cache, &recv] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(src.join("seed.txt"), b"seed-data").unwrap();

    // Tenant secret + cedar.
    let tenant_secret = run.join("tenant.secret");
    let raw: [u8; 32] = {
        let mut s = [0u8; 32];
        for (i, b) in s.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(173).wrapping_add(7);
        }
        s
    };
    std::fs::write(&tenant_secret, hex::encode(raw)).unwrap();
    std::fs::set_permissions(&tenant_secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cedar = run.join("policy.cedar");
    std::fs::write(&cedar, CEDAR_POLICY).unwrap();

    let fs_sock = run.join("cfs-fs.sock");
    let broker_sock = run.join("broker.sock");
    let audit_log = run.join("audit.ndjson");
    let daemon_toml = run.join("contextfsd.toml");

    // The two "sandbox-side" local UDS paths the SDK's receive_uds will
    // bind. For Loopback these are symlinks back to fs_sock / broker_sock.
    let local_fs_uds = recv.join("local-fs.sock");
    let local_broker_uds = recv.join("local-broker.sock");

    let spec = EmbedderTomlSpec {
        tenant_secret_path: tenant_secret.clone(),
        audit_log_path: audit_log.clone(),
        cedar_policy_path: cedar.clone(),
        principal: r#"Agent::"pi-spike""#.into(),
        broker_socket_path: local_broker_uds.clone(),
        mount_name: "work".into(),
        mountpoint: mnt.clone(),
        cache_dir: cache.clone(),
        remote_fs_target_uds: local_fs_uds.clone(),
        fuse_acl: FuseAcl::Auto,
        read_only: false,
    };
    std::fs::write(&daemon_toml, spec.render()).unwrap();

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
        "cfs-fs-server socket"
    );
    assert!(
        wait_for_socket(&broker_sock, Duration::from_secs(5)).await,
        "broker socket"
    );

    // ── expose + receive via the cfs-mesh SDK (Loopback transport) ──
    let (fs_blob, _fs_bridge) = expose_uds(&fs_sock, Transport::Loopback)
        .await
        .expect("expose_uds fs");
    let (broker_blob, _broker_bridge) = expose_uds(&broker_sock, Transport::Loopback)
        .await
        .expect("expose_uds broker");

    // ConnectionBlob serde round-trip — the contract pi-rs ships across
    // the host/sandbox boundary.
    let fs_blob_json = serde_json::to_string(&fs_blob).expect("serialize fs blob");
    let broker_blob_json = serde_json::to_string(&broker_blob).expect("serialize broker blob");
    let fs_blob: contextfs_mesh::blob::ConnectionBlob =
        serde_json::from_str(&fs_blob_json).expect("round-trip fs");
    let broker_blob: contextfs_mesh::blob::ConnectionBlob =
        serde_json::from_str(&broker_blob_json).expect("round-trip broker");

    let _fs_listener = receive_uds(&fs_blob, &local_fs_uds)
        .await
        .expect("receive_uds fs");
    let _broker_listener = receive_uds(&broker_blob, &local_broker_uds)
        .await
        .expect("receive_uds broker");

    assert!(
        wait_for_socket(&local_fs_uds, Duration::from_secs(3)).await,
        "local fs uds not bound by receive_uds"
    );
    assert!(
        wait_for_socket(&local_broker_uds, Duration::from_secs(3)).await,
        "local broker uds not bound by receive_uds"
    );

    // ── start contextfsd targeting the SDK-bound local UDSes ──
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
        "/work mount at {} never appeared",
        mnt.display()
    );

    // ── invariants — same shape as contextfs_local_rw_mount but data
    // flows through the SDK's loopback symlink, exercising the call wiring.
    let seen = std::fs::read(mnt.join("seed.txt")).expect("read seed.txt");
    assert_eq!(seen, b"seed-data");

    let payload = format!("via-sdk-{}", now_nanos());
    std::fs::write(mnt.join("written.txt"), &payload).unwrap();
    let host_seen = std::fs::read_to_string(src.join("written.txt")).expect("host sees write");
    assert_eq!(host_seen, payload, "mount→host write through SDK loopback");

    // Round-trip a small binary blob to be sure no encoding got in the way.
    let payload2: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
    std::fs::write(mnt.join("bin.dat"), &payload2).unwrap();
    let host_bin = std::fs::read(src.join("bin.dat")).expect("host sees bin");
    assert_eq!(host_bin, payload2, "binary round-trip");
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
