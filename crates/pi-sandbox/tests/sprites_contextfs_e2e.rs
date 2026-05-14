//! End-to-end RFD 0026 v2 — contextfs RW `/work` inside a real Sprite, with
//! the host↔sandbox transport mediated by RFD 0029's
//! `Transport::Agora` (cfs-mesh expose_uds / receive_uds).
//!
//! Hits the Sprites API. Provisions a fresh sprite, uploads the
//! contextfsd + cfs-mesh musl-static binaries, wires the tunnel,
//! mounts /work, verifies a write-through-FUSE → read-on-host round
//! trip, then destroys the sprite. Cleans up even on assertion
//! failure.
//!
//! Gate (skipped cleanly when any prerequisite is missing):
//!   - SPRITES_TOKEN (Sprites API key)
//!   - PI_SANDBOX_CONTEXTFSD_BIN, PI_SANDBOX_CFS_MESH_BIN,
//!     PI_SANDBOX_CFS_FS_SERVER_BIN, PI_SANDBOX_CONTEXTFS_BROKER_BIN
//!     (musl-static contextfs binaries built under
//!     contextfs/target/x86_64-unknown-linux-musl/release/)
//!   - PI_SANDBOX_WROMM_JSON pointing at a wromm.json spec (or
//!     `wromm.json` in CWD; the test creates a minimal one if missing).
//!
//! Cost: one Sprite session per run. ~30 s provision + 30 s bootstrap
//! + ~5 s teardown. Don't add this to plain `cargo test` — it is a
//! sandbox-required heavyweight test.
//!
//! Known upstream issue (2026-05-14): `cfs-mesh receive-uds` CLI wipes
//! the ephemeral agora identity dir before the listener subprocess
//! reads it (`into_child()` consumes the `ListenerHandle`, dropping the
//! held `TempDir`; the still-running `agora-listen` child can't find
//! `rooms.json` and errors "agora room not joined locally"). Fix
//! pending on contextfs branch rfd/0029. Reported in agora room
//! cfs-rfd-0029 message [a8e357]. This test goes green once
//! contextfs-mesh's `receive_uds_cli` keeps the identity dir alive
//! while waiting on the child.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use contextfs_mesh::blob::Transport;
use contextfs_mesh::expose_uds_with_seed;
use pi_sandbox::remote::sprites_contextfs::{bootstrap_contextfs, SpriteBootstrap};
use wromm::provider::Provider;
use wromm::provider_factory;
use wromm::sdk::WrommClient;

fn skip(reason: &str) {
    eprintln!("SKIP: sprites_contextfs_e2e: {reason}");
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

const WROMM_JSON: &str = r#"{
  "name": "pi-rs-sprites-contextfs-e2e",
  "runtimes": [{ "name": "rust", "version": "1.85" }],
  "system_packages": [],
  "services": [],
  "ports": [],
  "env": {},
  "source": { "type": "Manual" },
  "agent": null
}"#;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sprites_contextfs_rw_round_trip() {
    if std::env::var("SPRITES_TOKEN").is_err() {
        skip("SPRITES_TOKEN not set");
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
    let Some(cfs_mesh_bin) = resolve_bin("PI_SANDBOX_CFS_MESH_BIN", "cfs-mesh") else {
        skip("cfs-mesh not found");
        return;
    };

    // Workspace dir: holds wromm.json + host-side project ("the cwd the
    // sprite sees as /work" — populated with seed files we'll read from
    // inside the sprite).
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = tmp.path();
    std::fs::write(workspace.join("wromm.json"), WROMM_JSON).unwrap();

    let src = workspace.join("project");
    std::fs::create_dir_all(&src).unwrap();
    let host_seed = format!("host-seed-{}", now_nanos());
    std::fs::write(src.join("host-seed.txt"), &host_seed).unwrap();

    // Host-side run dir.
    let run = workspace.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let fs_sock = run.join("cfs-fs.sock");
    let broker_sock = run.join("broker.sock");
    let cedar = run.join("policy.cedar");
    let tenant_secret_path = run.join("tenant.secret");
    let raw: [u8; 32] = {
        let mut s = [0u8; 32];
        for (i, b) in s.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(173).wrapping_add(7);
        }
        s
    };
    let tenant_secret_hex = hex::encode(raw);
    std::fs::write(&tenant_secret_path, &tenant_secret_hex).unwrap();
    std::fs::set_permissions(&tenant_secret_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&cedar, CEDAR_POLICY).unwrap();

    // ── host-side cfs-fs-server + broker ──
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

    let _ = std::fs::remove_file(&broker_sock);
    let _broker_child = spawn_logging(
        Command::new(&broker_bin)
            .arg("run")
            .arg("--socket")
            .arg(&broker_sock)
            .arg("--policy")
            .arg(&cedar)
            .arg("--tenant-secret-path")
            .arg(&tenant_secret_path)
            .arg("--allowed-uid")
            .arg(format!("{}", nix_uid_self()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true),
    );

    assert!(wait_for_socket(&fs_sock, Duration::from_secs(5)).await);
    assert!(wait_for_socket(&broker_sock, Duration::from_secs(5)).await);

    // ── provision sprite ──
    // Make sure cfs-mesh is locatable by contextfs-mesh's expose_uds.
    // The agora-bridge subprocess it spawns needs this binary.
    std::env::set_var("CFS_MESH_BIN", &cfs_mesh_bin);

    let client = WrommClient::new(workspace);
    let provider = provider_factory::create_provider("sprites")
        .expect("create_provider sprites");
    let spec = client.load_spec("wromm.json").expect("load wromm.json");

    let wromm = provider.provision(&spec).await.expect("provision sprite");
    let sandbox_id = wromm.id.clone();
    eprintln!("provisioned sprite: id={sandbox_id}");

    // Best-effort teardown wrapper.
    let outcome = run_test_body(
        provider.as_ref(),
        &sandbox_id,
        &fs_sock,
        &broker_sock,
        &contextfsd_bin,
        &cfs_mesh_bin,
        &tenant_secret_hex,
        &src,
        &host_seed,
    )
    .await;

    if let Err(e) = provider.destroy(&sandbox_id).await {
        eprintln!("destroy failed (sprite may leak): {e}");
    }

    outcome.expect("test body");
}

#[allow(clippy::too_many_arguments)]
async fn run_test_body(
    provider: &dyn Provider,
    sandbox_id: &str,
    fs_sock: &Path,
    broker_sock: &Path,
    contextfsd_bin: &Path,
    cfs_mesh_bin: &Path,
    tenant_secret_hex: &str,
    src_dir: &Path,
    host_seed: &str,
) -> Result<(), anyhow::Error> {
    // ── expose both host UDSes via Agora transport ──
    let seed_fs = format!("{sandbox_id}-fs").into_bytes();
    let seed_broker = format!("{sandbox_id}-broker").into_bytes();
    let (fs_blob, _fs_bridge) = expose_uds_with_seed(fs_sock, Transport::Agora, &seed_fs)
        .await
        .map_err(|e| anyhow::anyhow!("expose_uds fs: {e}"))?;
    let (broker_blob, _broker_bridge) =
        expose_uds_with_seed(broker_sock, Transport::Agora, &seed_broker)
            .await
            .map_err(|e| anyhow::anyhow!("expose_uds broker: {e}"))?;
    eprintln!("exposed fs + broker UDSes over agora");

    // ── sandbox-side bootstrap ──
    let bootstrap = SpriteBootstrap {
        contextfsd_host_path: contextfsd_bin.to_path_buf(),
        cfs_mesh_host_path: cfs_mesh_bin.to_path_buf(),
        tenant_secret_hex: tenant_secret_hex.to_string(),
        cedar_policy_text: CEDAR_POLICY.to_string(),
        fs_blob,
        broker_blob,
        recv_uds_timeout: Duration::from_secs(15),
        mount_timeout: Duration::from_secs(20),
    };

    let result = bootstrap_contextfs(provider, sandbox_id, &bootstrap).await;

    // If bootstrap failed, fetch diagnostics before returning the error.
    let result = match result {
        Ok(r) => Ok(r),
        Err(e) => {
            eprintln!("bootstrap_contextfs failed: {e}");
            dump_sprite_diagnostics(provider, sandbox_id).await;
            Err(anyhow::anyhow!("bootstrap_contextfs: {e}"))
        }
    }?;

    eprintln!("bootstrap_contextfs ok: {result:?}");

    // ── invariants ──
    let r = provider
        .exec(sandbox_id, &["cat", "/work/host-seed.txt"])
        .await
        .map_err(|e| anyhow::anyhow!("exec cat: {e}"))?;
    if r.exit_code != 0 {
        dump_sprite_diagnostics(provider, sandbox_id).await;
        return Err(anyhow::anyhow!(
            "cat /work/host-seed.txt: exit={} stderr={:?}",
            r.exit_code,
            String::from_utf8_lossy(&r.stderr)
        ));
    }
    let seen = String::from_utf8_lossy(&r.stdout).trim().to_string();
    if seen != host_seed {
        return Err(anyhow::anyhow!(
            "host-seed mismatch: expected {host_seed:?}, sprite saw {seen:?}"
        ));
    }
    eprintln!("invariant 1 ok: host seed visible at /work/host-seed.txt");

    // Write a file inside the sprite at /work; verify it shows up on host.
    let sprite_payload = format!("sprite-write-{}", now_nanos());
    let write_cmd = format!("echo {sp} > /work/sprite-write.txt", sp = sprite_payload);
    let r = provider
        .exec(sandbox_id, &["sh", "-c", &write_cmd])
        .await
        .map_err(|e| anyhow::anyhow!("exec write: {e}"))?;
    if r.exit_code != 0 {
        dump_sprite_diagnostics(provider, sandbox_id).await;
        return Err(anyhow::anyhow!(
            "write to /work: exit={} stderr={:?}",
            r.exit_code,
            String::from_utf8_lossy(&r.stderr)
        ));
    }
    let host_view = std::fs::read_to_string(src_dir.join("sprite-write.txt"))
        .map_err(|e| anyhow::anyhow!("host read sprite-write.txt: {e}"))?;
    if host_view.trim() != sprite_payload {
        return Err(anyhow::anyhow!(
            "sprite write not seen on host: expected {sprite_payload:?}, host sees {host_view:?}"
        ));
    }
    eprintln!("invariant 2 ok: sprite→host write propagates");

    Ok(())
}

async fn dump_sprite_diagnostics(provider: &dyn Provider, sandbox_id: &str) {
    for cmd in [
        vec!["ls", "-la", "/home/sprite/"],
        vec!["sudo", "-n", "ls", "-la", "/etc/contextfs/"],
        vec!["sudo", "-n", "cat", "/etc/contextfs/contextfsd.toml"],
        vec!["mountpoint", "/work"],
        vec!["sudo", "-n", "tail", "-50", "/var/log/contextfsd.log"],
        vec!["sudo", "-n", "tail", "-30", "/var/log/cfs-mesh-recv-fs.log"],
        vec!["sudo", "-n", "tail", "-30", "/var/log/cfs-mesh-recv-broker.log"],
        vec!["which", "fusermount3"],
        vec!["sudo", "-n", "ps", "auxf"],
    ] {
        let r = provider.exec(sandbox_id, &cmd).await;
        eprintln!("--- diag: {:?} ---", cmd);
        match r {
            Ok(r) => {
                eprintln!("exit={}", r.exit_code);
                eprint!("{}", String::from_utf8_lossy(&r.stdout));
                if !r.stderr.is_empty() {
                    eprintln!("[stderr] {}", String::from_utf8_lossy(&r.stderr));
                }
            }
            Err(e) => eprintln!("exec err: {e}"),
        }
    }
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
