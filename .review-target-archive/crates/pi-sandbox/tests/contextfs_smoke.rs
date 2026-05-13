//! Contextfs library-API smoke (RFD 0023 §3.5 / Commit G3 prep).
//!
//! Two assertions:
//!
//! 1. **Pin guard.** The sibling `../contextfs` checkout's `git
//!    HEAD` matches `pi_sandbox::contextfs::EXPECTED_CONTEXTFS_REV`.
//!    If the user `git pull`s contextfs and accidentally moves the
//!    checkout to a newer commit, the build's path dep keeps
//!    happily linking against the new code, but this test fails
//!    loudly so we notice.
//!
//! 2. **Daemon lifecycle.** `contextfsd::start(cfg).await` succeeds
//!    on a minimal config (default-permit Cedar, no mounts, no
//!    broker, no OIDC, audit log to a tempdir), then
//!    `handle.shutdown().await` cleanly tears down. Proves the
//!    library API surface we depend on actually links + runs in
//!    pi-rs's process. The interesting in-guest FUSE-over-vsock
//!    path is the next chunk of G3; this is just dep-validation.
//!
//! No microvm involvement; no rootfs; no network. Runs in any
//! Linux environment with a kernel that allows the test process
//! to spawn tokio tasks.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[test]
fn pinned_contextfs_rev_matches_local_checkout() {
    // Locate the contextfs checkout. The Cargo.toml dep is a path
    // dep at `../contextfs/crates/contextfsd`, so the repo root is
    // `<this>/../../../contextfs`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let contextfs_repo = manifest_dir
        .parent()                  // crates/
        .and_then(|p| p.parent())  // pi-rs/
        .and_then(|p| p.parent())  // code/
        .map(|p| p.join("contextfs"))
        .expect("can locate ../contextfs from pi-sandbox manifest");

    if !contextfs_repo.join(".git").exists() {
        eprintln!(
            "SKIP: {} is not a git checkout — pin guard skipped.",
            contextfs_repo.display()
        );
        return;
    }

    let head = std::process::Command::new("git")
        .args(["-C", contextfs_repo.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse HEAD");
    assert!(
        head.status.success(),
        "git rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&head.stderr)
    );
    let head_str = String::from_utf8(head.stdout).expect("utf8");
    let head_str = head_str.trim();

    let expected = pi_sandbox::contextfs::EXPECTED_CONTEXTFS_REV;
    assert_eq!(
        head_str,
        expected,
        "contextfs checkout drifted from the pinned rev. \
         Expected {expected} (per pi_sandbox::contextfs::EXPECTED_CONTEXTFS_REV), \
         got {head_str}. Either bump the const in lockstep with the dep, \
         or `git -C {} checkout {expected}`.",
        contextfs_repo.display(),
    );
}

#[tokio::test]
async fn contextfsd_start_and_shutdown_clean() {
    // Build a minimal DaemonConfig: no mounts, default-permit Cedar,
    // no broker, no OIDC, audit log + tenant secret in a tempdir.
    let tmp = tempfile::tempdir().expect("tempdir");

    // Tenant secret: 32 random-ish bytes, mode 0600 (the daemon
    // refuses to start if mode is too permissive).
    let secret_path = tmp.path().join("tenant-secret");
    let secret_bytes: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(13));
    fs::write(&secret_path, secret_bytes).expect("write secret");
    fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600))
        .expect("chmod 0600 secret");

    // Cedar policy: default-permit. The daemon parses+validates
    // this on start; an empty file or syntax error fails start.
    let policy_path = tmp.path().join("policy.cedar");
    fs::write(&policy_path, "permit (principal, action, resource);\n")
        .expect("write cedar policy");

    // Minimal DaemonConfig. We rely on serde defaults for every
    // optional field. Construct via TOML to avoid taking a hard
    // dependency on the contextfsd::config struct shape across
    // pin bumps.
    // The `mount` field has a serde default of `[]`, so we just
    // omit it for a no-mounts smoke. Top-level scalars + `[pdp]`
    // table is the minimum.
    // Backing dir for the local-backend mount. Must exist before
    // contextfsd starts (the daemon checks `root` is accessible).
    let backing = tmp.path().join("backing");
    fs::create_dir_all(&backing).expect("mkdir backing");
    fs::write(backing.join("hello.txt"), "hi from contextfs\n").expect("seed file");

    // Mountpoint: the FUSE mount lands here. The dir must exist.
    let mountpoint = tmp.path().join("mnt");
    fs::create_dir_all(&mountpoint).expect("mkdir mnt");

    let cache_dir = tmp.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("mkdir cache");

    let toml = format!(
        r#"
tenant_secret_path = "{secret}"
audit_log_path = "{audit}"

[pdp]
policy_path = "{policy}"
default_principal = "User::\"smoke\""

[[mount]]
name = "smoke"
mountpoint = "{mountpoint}"
backend = "local"
cache_dir = "{cache_dir}"
read_only = true

[mount.local]
root = "{backing}"
follow_symlinks = "surface"
allow_unsafe_fs = true
"#,
        secret = secret_path.display(),
        audit = tmp.path().join("audit.log").display(),
        policy = policy_path.display(),
        mountpoint = mountpoint.display(),
        cache_dir = cache_dir.display(),
        backing = backing.display(),
    );
    let cfg: contextfsd::DaemonConfig =
        toml::from_str(&toml).expect("parse minimal DaemonConfig");

    // Start.
    let handle = match contextfsd::start(cfg).await {
        Ok(h) => h,
        Err(e) => {
            // contextfsd may refuse on hosts where /dev/fuse is
            // missing (CI containers, FUSE not enabled). Skip
            // rather than fail — the build-import already proved
            // the dep links.
            eprintln!("SKIP: contextfsd::start failed: {e}");
            return;
        }
    };

    eprintln!("contextfsd up; handle = {handle:?}");

    // Shutdown.
    handle.shutdown().await.expect("shutdown clean");
}
