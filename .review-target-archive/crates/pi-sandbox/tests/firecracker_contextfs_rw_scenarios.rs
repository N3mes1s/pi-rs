//! Multi-scenario RW `/work` integration tests for pi-rs's
//! microVM + contextfs stack (RFD 0023 / Commit G3 step 3).
//!
//! Each test exercises a real-shape agent workflow against a
//! freshly-acquired VM, driving the FUSE write path through the
//! Cedar broker over vsock(2,5006). Together they cover the
//! capabilities a coding agent typically uses inside a sandbox:
//!
//! * `rw_workflow_create_modify_verify` — write a source tree,
//!   modify a file in place, read both versions back. The
//!   single-file write path most agents hit on every turn.
//! * `rw_subdirectory_create_and_populate` — mkdir + nested
//!   files. Exercises `Action::"create"` on directories.
//! * `rw_large_payload_byte_identical` — 64 KiB blob write +
//!   read-back; bounds-tests the FUSE write fragmentation +
//!   contextfs's wire-envelope buffer.
//! * `rw_overwrite_existing_file` — write A, then overwrite to
//!   B; host sees the latest bytes. Verifies the broker
//!   permits successive Action::"write" against the same path.
//! * `rw_delete_file_via_agent` — rm a file from the guest,
//!   host's view of host_cwd no longer has it. Exercises
//!   Action::"delete".
//!
//! All five share the same prereq gates as
//! `firecracker_contextfs_rw_mount`:
//!   PI_SANDBOX_FC_TEST=1, PI_SANDBOX_CONTEXTFS_RW=1,
//!   PI_SANDBOX_KERNEL, PI_SANDBOX_ROOTFS,
//!   PI_SANDBOX_CFS_FS_SERVER_BIN, PI_SANDBOX_CONTEXTFS_BROKER_BIN,
//!   /dev/kvm openable RW, firecracker on PATH.
//! Skip cleanly when any prereq is absent.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::launcher::MicroVmLauncher;
use pi_sandbox::microvm::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use pi_tools::ToolContext;
use serde_json::json;

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

/// Resolve every prereq in one place. Returns `None` (with a
/// `SKIP:` message printed) when something's missing.
struct Prereqs {
    kernel: PathBuf,
    rootfs: PathBuf,
}

fn check_prereqs() -> Option<Prereqs> {
    if std::env::var("PI_SANDBOX_FC_TEST").ok().filter(|s| !s.is_empty()).is_none() {
        skip("PI_SANDBOX_FC_TEST not set");
        return None;
    }
    if std::env::var("PI_SANDBOX_CONTEXTFS_RW").ok().as_deref() != Some("1") {
        skip("PI_SANDBOX_CONTEXTFS_RW=1 not set — RW path opt-in");
        return None;
    }
    if which::which("firecracker").is_err() {
        skip("firecracker not on PATH");
        return None;
    }

    let cfs = std::env::var("PI_SANDBOX_CFS_FS_SERVER_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists());
    if cfs.is_none() && which::which("cfs-fs-server").is_err() {
        skip("cfs-fs-server not resolvable");
        return None;
    }
    let broker = std::env::var("PI_SANDBOX_CONTEXTFS_BROKER_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists());
    if broker.is_none() && which::which("contextfs-broker").is_err() {
        skip("contextfs-broker not resolvable");
        return None;
    }

    let kernel = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_KERNEL not set");
            return None;
        }
    };
    if !kernel.exists() {
        skip(&format!("PI_SANDBOX_KERNEL={} missing", kernel.display()));
        return None;
    }
    let rootfs = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            skip("PI_SANDBOX_ROOTFS not set");
            return None;
        }
    };
    if !rootfs.exists() {
        skip(&format!("PI_SANDBOX_ROOTFS={} missing", rootfs.display()));
        return None;
    }
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_err()
    {
        skip("/dev/kvm not openable RW");
        return None;
    }

    Some(Prereqs { kernel, rootfs })
}

/// Stage a host_cwd tempdir with mode 0777 and return its
/// handle (kept alive by the caller for the test's lifetime).
fn staged_host_cwd() -> tempfile::TempDir {
    let host_cwd = tempfile::tempdir().expect("host_cwd tempdir");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(
        host_cwd.path(),
        std::fs::Permissions::from_mode(0o777),
    );
    host_cwd
}

/// Acquire a fresh VM rooted at `host_cwd` (RW). Boilerplate
/// shared across every scenario in this file. Returns the
/// handle plus the launcher (held by the caller so the warm
/// pool isn't dropped while the handle is still in use).
async fn acquire_vm(
    pre: &Prereqs,
    host_cwd: &std::path::Path,
) -> (
    Box<dyn pi_sandbox::microvm::launcher::VmHandle>,
    tempfile::TempDir,
) {
    let run_root = tempfile::tempdir().expect("run_dir tempdir");
    let cfg = FirecrackerConfig {
        kernel_path: Some(pre.kernel.clone()),
        rootfs_path: Some(pre.rootfs.clone()),
        run_dir: run_root.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(cfg);
    let report = launcher.probe().await.expect("probe Ok");
    assert!(report.available, "probe not available: {report:?}");

    let spec = VmSpec {
        host_cwd: host_cwd.to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };
    let handle = launcher.acquire(&spec).await.expect("acquire");
    (handle, run_root)
}

async fn run_bash(
    handle: &dyn pi_sandbox::microvm::launcher::VmHandle,
    cmd: &str,
) -> pi_sandbox::microvm::VmExecution {
    let ctx = ToolContext::default();
    let limits = CallLimits::default();
    handle
        .execute(&ctx, &limits, "bash", &json!({ "command": cmd }))
        .await
        .expect("execute bash")
}

// ────────────────────────────────────────────────────────────
// Scenario 1: write a tiny file, modify it in place, read both
// versions back. Most-frequent agent shape (edit a source file
// twice in one turn).
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn rw_workflow_create_modify_verify() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_host_cwd();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    let v1 = run_bash(
        &*handle,
        "printf 'version-1: tomato' > /work/note.txt && cat /work/note.txt",
    )
    .await;
    assert!(!v1.result.is_error, "v1 write/read failed: {}", v1.result.model_output);
    assert!(
        v1.result.model_output.contains("version-1: tomato"),
        "expected v1 read-back, got: {:?}",
        v1.result.model_output
    );

    let v2 = run_bash(
        &*handle,
        "printf 'version-2: parsnip' > /work/note.txt && cat /work/note.txt",
    )
    .await;
    assert!(!v2.result.is_error, "v2 write/read failed: {}", v2.result.model_output);
    assert!(
        v2.result.model_output.contains("version-2: parsnip"),
        "expected v2 read-back, got: {:?}",
        v2.result.model_output
    );

    // Host sees only the latest version.
    let host_view = std::fs::read_to_string(host_cwd.path().join("note.txt"))
        .expect("host read note.txt");
    assert_eq!(
        host_view, "version-2: parsnip",
        "host saw {host_view:?}, expected the v2 bytes"
    );

    handle.release().await.expect("release");
    eprintln!("rw_workflow_create_modify_verify PASSED");
}

// ────────────────────────────────────────────────────────────
// Scenario 2: mkdir + nested file operations. Action::"create"
// on directories + write into them.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn rw_subdirectory_create_and_populate() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_host_cwd();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    let exec = run_bash(
        &*handle,
        "mkdir -p /work/src/nested && \
         printf 'mod top;' > /work/src/lib.rs && \
         printf 'pub fn add(a:i32,b:i32)->i32{a+b}' > /work/src/nested/math.rs && \
         echo BUILT_OK",
    )
    .await;
    assert!(!exec.result.is_error, "mkdir+writes failed: {}", exec.result.model_output);
    assert!(
        exec.result.model_output.contains("BUILT_OK"),
        "expected BUILT_OK after mkdir+writes, got: {:?}",
        exec.result.model_output
    );

    // Host sees the directory + both files.
    assert!(host_cwd.path().join("src").is_dir(), "src/ missing on host");
    assert!(
        host_cwd.path().join("src/nested").is_dir(),
        "src/nested/ missing on host"
    );
    let lib = std::fs::read_to_string(host_cwd.path().join("src/lib.rs"))
        .expect("host read src/lib.rs");
    assert_eq!(lib, "mod top;");
    let math = std::fs::read_to_string(host_cwd.path().join("src/nested/math.rs"))
        .expect("host read src/nested/math.rs");
    assert!(
        math.contains("pub fn add"),
        "host saw nested file: {math:?}"
    );

    handle.release().await.expect("release");
    eprintln!("rw_subdirectory_create_and_populate PASSED");
}

// ────────────────────────────────────────────────────────────
// Scenario 3: 64 KiB blob round-trip in both directions.
// Bounds-tests the FUSE write fragmentation + contextfs's
// MAX_WIRE_BYTES envelope (64 MiB; 64 KiB is well inside).
//
// (a) Host stages a deterministic 64 KiB blob; guest reads via
//     `wc -c` + `sha256sum` and we compare the digest to the
//     value we computed on the host. Proves the FUSE read path
//     gives byte-identical data on a chunk that crosses several
//     FUSE max_read boundaries.
// (b) Guest generates a fresh 64 KiB blob with `dd` from
//     /dev/urandom, host reads back. Proves the FUSE write
//     path supports >1 chunk too.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn rw_large_payload_byte_identical() {
    use sha2::{Digest, Sha256};

    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_host_cwd();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    // (a) host → guest read.
    let target_len: usize = 64 * 1024;
    let mut payload = Vec::with_capacity(target_len);
    let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    let mut col = 0usize;
    let mut idx = 0usize;
    while payload.len() < target_len {
        payload.push(alphabet[idx % alphabet.len()]);
        idx += 1;
        col += 1;
        if col == 80 {
            payload.push(b'\n');
            col = 0;
        }
    }
    payload.truncate(target_len);
    let host_blob = host_cwd.path().join("host_seeded.bin");
    std::fs::write(&host_blob, &payload).expect("host seed blob");

    let mut hasher = Sha256::new();
    hasher.update(&payload);
    let host_digest = hex::encode(hasher.finalize());

    let guest_csum = run_bash(
        &*handle,
        "wc -c /work/host_seeded.bin && sha256sum /work/host_seeded.bin",
    )
    .await;
    assert!(
        !guest_csum.result.is_error,
        "guest checksum failed: {}",
        guest_csum.result.model_output
    );
    let guest_view = guest_csum.result.model_output.clone();
    assert!(
        guest_view.contains(&format!("{target_len}")),
        "guest wc -c didn't see {target_len} bytes; got: {guest_view:?}"
    );
    assert!(
        guest_view.contains(&host_digest),
        "guest sha256 digest mismatch — host computed {host_digest}, \
         guest output: {guest_view:?}"
    );

    // (b) guest → host write. 64 KiB random, generated fresh
    //     in the guest with dd; host reads back and verifies the
    //     length plus the guest-reported sha256 matches its own
    //     re-computation.
    let exec = run_bash(
        &*handle,
        "dd if=/dev/urandom of=/work/guest_made.bin bs=1024 count=64 2>/dev/null && \
         wc -c /work/guest_made.bin && sha256sum /work/guest_made.bin",
    )
    .await;
    assert!(
        !exec.result.is_error,
        "guest dd write failed: {}",
        exec.result.model_output
    );
    let out = exec.result.model_output.clone();
    assert!(
        out.contains(&format!("{target_len}")),
        "guest wc -c on guest_made.bin didn't see {target_len}; got: {out:?}"
    );
    // Extract the guest-side sha256 (first 64 hex chars in the
    // sha256sum output).
    let guest_digest = out
        .split_whitespace()
        .find(|tok| tok.len() == 64 && tok.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|s| s.to_string())
        .expect("could not parse guest sha256 from output");

    let host_bytes = std::fs::read(host_cwd.path().join("guest_made.bin"))
        .expect("host read guest_made.bin");
    assert_eq!(
        host_bytes.len(),
        target_len,
        "host sees {} bytes, expected {target_len}",
        host_bytes.len()
    );
    let mut hasher = Sha256::new();
    hasher.update(&host_bytes);
    let host_recompute = hex::encode(hasher.finalize());
    assert_eq!(
        host_recompute, guest_digest,
        "host sha256 != guest sha256 — guest wrote different bytes than host sees"
    );

    handle.release().await.expect("release");
    eprintln!(
        "rw_large_payload_byte_identical PASSED ({target_len} bytes both directions)"
    );
}

// ────────────────────────────────────────────────────────────
// Scenario 4: write, overwrite, verify only the latest bytes
// are visible. Verifies the broker permits successive
// Action::"write" against the same path AND the kernel cache
// doesn't serve a stale read.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn rw_overwrite_existing_file() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_host_cwd();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    let r1 = run_bash(
        &*handle,
        "printf 'first' > /work/f.txt && cat /work/f.txt",
    )
    .await;
    assert!(r1.result.model_output.contains("first"), "first read: {:?}", r1.result.model_output);

    let r2 = run_bash(
        &*handle,
        "printf 'second-longer-than-first' > /work/f.txt && cat /work/f.txt && wc -c /work/f.txt",
    )
    .await;
    assert!(
        r2.result.model_output.contains("second-longer-than-first"),
        "second read: {:?}",
        r2.result.model_output
    );
    // 24 chars; ensure no leftover bytes from "first" tail.
    assert!(
        r2.result.model_output.contains("24"),
        "expected wc=24, got: {:?}",
        r2.result.model_output
    );

    let host_view = std::fs::read_to_string(host_cwd.path().join("f.txt"))
        .expect("host read f.txt");
    assert_eq!(host_view, "second-longer-than-first");

    handle.release().await.expect("release");
    eprintln!("rw_overwrite_existing_file PASSED");
}

// ────────────────────────────────────────────────────────────
// Scenario 5: agent rm a file; host's view of host_cwd no
// longer has it. Exercises Action::"delete" through the
// broker.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn rw_delete_file_via_agent() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_host_cwd();
    // Host pre-stages two files; agent removes one, host
    // verifies one is gone and one remains.
    std::fs::write(host_cwd.path().join("keep.txt"), "stays").expect("seed keep");
    std::fs::write(host_cwd.path().join("drop.txt"), "goes").expect("seed drop");

    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    let exec = run_bash(
        &*handle,
        "ls /work/ && rm /work/drop.txt && echo REMOVED && ls /work/",
    )
    .await;
    assert!(!exec.result.is_error, "rm failed: {}", exec.result.model_output);
    assert!(
        exec.result.model_output.contains("REMOVED"),
        "expected REMOVED marker, got: {:?}",
        exec.result.model_output
    );
    // The post-rm `ls /work/` chunk must not contain drop.txt.
    let post = exec
        .result
        .model_output
        .split("REMOVED")
        .nth(1)
        .unwrap_or("");
    assert!(
        !post.contains("drop.txt"),
        "drop.txt still listed in post-rm ls: {post:?}"
    );
    assert!(
        post.contains("keep.txt"),
        "keep.txt should still be in post-rm ls: {post:?}"
    );

    // Host: drop.txt gone, keep.txt still there.
    assert!(
        !host_cwd.path().join("drop.txt").exists(),
        "drop.txt still on host after agent rm"
    );
    assert!(
        host_cwd.path().join("keep.txt").exists(),
        "keep.txt should not have been removed"
    );

    handle.release().await.expect("release");
    eprintln!("rw_delete_file_via_agent PASSED");
}
