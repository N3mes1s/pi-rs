//! Tests-only Cedar profile against a fake source-tree mount.
//!
//! Mirrors the dogfood pattern "agent reads the whole source
//! tree but can only write into `tests/`". The host stages a
//! fake source tree under host_cwd, boots a microVM with
//! `PI_SANDBOX_CEDAR_PROFILE=tests_only`, then drives the bash
//! tool through three scenarios:
//!
//! * `tests_only_read_anywhere` — `cat` files under
//!   `src/`, `crates/<x>/src/`, top-level `Cargo.toml`. All
//!   succeed (Cedar `Action::"read"` permitted everywhere).
//! * `tests_only_write_to_src_is_denied` — agent tries to
//!   modify `src/lib.rs`. `Action::"write"` is denied by
//!   Cedar; the bash command exits non-zero. Host's view of
//!   `src/lib.rs` is unchanged.
//! * `tests_only_write_to_tests_dir_is_allowed` — agent
//!   creates `tests/agent_added_test.rs`. `Action::"create"`
//!   matches `*/tests/*`; the host sees the new file with the
//!   guest-written bytes.
//!
//! Together these prove the Cedar `like`-pattern path matching
//! works against the broker over vsock(2,5006), and that the
//! "write tests but not the impl" guarantee is real (not just
//! suggested by a docstring).

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

struct Prereqs {
    kernel: PathBuf,
    rootfs: PathBuf,
}

fn check_prereqs() -> Option<Prereqs> {
    if std::env::var("PI_SANDBOX_FC_TEST")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        skip("PI_SANDBOX_FC_TEST not set");
        return None;
    }
    if std::env::var("PI_SANDBOX_CONTEXTFS_RW").ok().as_deref() != Some("1") {
        skip("PI_SANDBOX_CONTEXTFS_RW=1 not set — RW path opt-in");
        return None;
    }
    if std::env::var("PI_SANDBOX_CEDAR_PROFILE").ok().as_deref() != Some("tests_only") {
        skip("PI_SANDBOX_CEDAR_PROFILE=tests_only not set");
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

/// Stage a fake source-tree under `host_cwd` mode 0777, with:
///   src/lib.rs                — implementation file (NOT writable)
///   src/inner/mod.rs          — implementation file (NOT writable)
///   Cargo.toml                — manifest (NOT writable)
///   tests/existing_test.rs    — test file (writable)
fn staged_fake_tree() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("host_cwd tempdir");
    let _ = std::fs::set_permissions(
        dir.path(),
        std::fs::Permissions::from_mode(0o777),
    );

    std::fs::create_dir_all(dir.path().join("src/inner")).expect("mkdir src/inner");
    std::fs::create_dir_all(dir.path().join("tests")).expect("mkdir tests");

    std::fs::write(
        dir.path().join("src/lib.rs"),
        "// implementation — must remain untouched by tests-only profile\n\
         pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .expect("seed src/lib.rs");
    std::fs::write(
        dir.path().join("src/inner/mod.rs"),
        "pub fn mul(a: i32, b: i32) -> i32 { a * b }\n",
    )
    .expect("seed src/inner/mod.rs");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fake-tree\"\nversion = \"0.1.0\"\n",
    )
    .expect("seed Cargo.toml");
    std::fs::write(
        dir.path().join("tests/existing_test.rs"),
        "// existing test — present at acquire time\n",
    )
    .expect("seed tests/existing_test.rs");

    // Open up perms on staged files so the unprivileged
    // bash subprocess (UID 1001) can read/write them through
    // the FUSE mount even when caller_uid_passthrough doesn't
    // align with the host stat.
    let _ = std::fs::set_permissions(
        dir.path().join("src/lib.rs"),
        std::fs::Permissions::from_mode(0o666),
    );
    let _ = std::fs::set_permissions(
        dir.path().join("src/inner/mod.rs"),
        std::fs::Permissions::from_mode(0o666),
    );
    let _ = std::fs::set_permissions(
        dir.path().join("Cargo.toml"),
        std::fs::Permissions::from_mode(0o666),
    );
    let _ = std::fs::set_permissions(
        dir.path().join("tests/existing_test.rs"),
        std::fs::Permissions::from_mode(0o666),
    );

    dir
}

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
// Scenario A: read everywhere succeeds. Cedar permits
// `Action::"read"` for any resource path under the
// `tests_only` profile.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn tests_only_read_anywhere() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_fake_tree();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    let exec = run_bash(
        &*handle,
        "set -e; \
         cat /work/src/lib.rs && \
         cat /work/src/inner/mod.rs && \
         cat /work/Cargo.toml && \
         cat /work/tests/existing_test.rs && \
         echo READ_ALL_OK",
    )
    .await;
    assert!(
        !exec.result.is_error,
        "read-anywhere failed: {}",
        exec.result.model_output
    );
    assert!(
        exec.result.model_output.contains("READ_ALL_OK"),
        "expected READ_ALL_OK after reading impl + manifest + test files; got: {:?}",
        exec.result.model_output
    );
    // Spot-check we actually saw real content from each file.
    for needle in [
        "pub fn add",
        "pub fn mul",
        "fake-tree",
        "existing test",
    ] {
        assert!(
            exec.result.model_output.contains(needle),
            "expected to see {:?} in concat'd reads; got: {:?}",
            needle,
            exec.result.model_output
        );
    }

    handle.release().await.expect("release");
    eprintln!("tests_only_read_anywhere PASSED");
}

// ────────────────────────────────────────────────────────────
// Scenario B: write to /work/src/* is DENIED. Cedar rejects
// `Action::"write"` because the path doesn't match the
// tests-only `like` patterns. The bash command exits non-zero
// and the host sees the file unchanged.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn tests_only_write_to_src_is_denied() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_fake_tree();

    let original_lib = std::fs::read_to_string(host_cwd.path().join("src/lib.rs"))
        .expect("read seed lib.rs");
    let original_inner = std::fs::read_to_string(host_cwd.path().join("src/inner/mod.rs"))
        .expect("read seed inner/mod.rs");
    let original_cargo = std::fs::read_to_string(host_cwd.path().join("Cargo.toml"))
        .expect("read seed Cargo.toml");

    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    // Try four different write patterns, each of which should
    // be denied. The shell records each rc and reports back.
    let exec = run_bash(
        &*handle,
        "rc1=0; rc2=0; rc3=0; rc4=0; \
         (printf 'AGENT_TAMPERED' > /work/src/lib.rs)             2>/dev/null || rc1=$?; \
         (printf 'AGENT_TAMPERED' > /work/src/inner/mod.rs)       2>/dev/null || rc2=$?; \
         (printf 'AGENT_TAMPERED' > /work/Cargo.toml)             2>/dev/null || rc3=$?; \
         (printf 'AGENT_TAMPERED' > /work/src/new_impl.rs)        2>/dev/null || rc4=$?; \
         echo \"rc1=$rc1 rc2=$rc2 rc3=$rc3 rc4=$rc4\"",
    )
    .await;
    eprintln!(
        "[tests_only_write_to_src_is_denied] guest output: {}",
        exec.result.model_output
    );

    // Every attempt should have failed (rc != 0).
    for tag in ["rc1=", "rc2=", "rc3=", "rc4="] {
        // Find the rc value for this tag (must NOT be `=0`).
        let rc_chunk = exec
            .result
            .model_output
            .split_whitespace()
            .find(|tok| tok.starts_with(tag))
            .unwrap_or("");
        assert!(
            !rc_chunk.is_empty(),
            "missing {tag} marker in: {:?}",
            exec.result.model_output
        );
        assert_ne!(
            rc_chunk,
            format!("{tag}0"),
            "expected non-zero exit for {tag} (write to src/* should have been denied)"
        );
    }

    // Host bytes unchanged for all three pre-existing impl files.
    let after_lib = std::fs::read_to_string(host_cwd.path().join("src/lib.rs"))
        .expect("re-read lib.rs");
    assert_eq!(
        after_lib, original_lib,
        "src/lib.rs was modified despite tests_only profile"
    );
    let after_inner = std::fs::read_to_string(host_cwd.path().join("src/inner/mod.rs"))
        .expect("re-read inner/mod.rs");
    assert_eq!(
        after_inner, original_inner,
        "src/inner/mod.rs was modified despite tests_only profile"
    );
    let after_cargo = std::fs::read_to_string(host_cwd.path().join("Cargo.toml"))
        .expect("re-read Cargo.toml");
    assert_eq!(
        after_cargo, original_cargo,
        "Cargo.toml was modified despite tests_only profile"
    );
    // The new_impl.rs creation should not have produced a file.
    assert!(
        !host_cwd.path().join("src/new_impl.rs").exists(),
        "src/new_impl.rs was created despite tests_only profile"
    );

    handle.release().await.expect("release");
    eprintln!("tests_only_write_to_src_is_denied PASSED");
}

// ────────────────────────────────────────────────────────────
// Scenario C: write to /work/tests/* is ALLOWED. Cedar
// `*/tests/*` `like` pattern matches; create succeeds; host
// sees the new file with guest-written bytes.
// ────────────────────────────────────────────────────────────
#[tokio::test]
async fn tests_only_write_to_tests_dir_is_allowed() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_fake_tree();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    // Heredoc with single-quoted delimiter so the shell doesn't
    // expand `$2` / `${...}` inside the test body. Newlines come
    // from the literal Rust string (real `\n` bytes, not the
    // escape sequence).
    let exec = run_bash(
        &*handle,
        "cat > /work/tests/agent_added_test.rs <<'EOF'
// agent-added test
#[test] fn agent_can_write_here() { assert_eq!(2 + 2, 4); }
EOF
cat /work/tests/agent_added_test.rs && echo TESTS_WRITE_OK",
    )
    .await;
    eprintln!(
        "[tests_only_write_to_tests_dir_is_allowed] guest output: {}",
        exec.result.model_output
    );
    assert!(
        !exec.result.is_error,
        "write to /work/tests/agent_added_test.rs failed: {}",
        exec.result.model_output
    );
    assert!(
        exec.result.model_output.contains("TESTS_WRITE_OK"),
        "expected TESTS_WRITE_OK marker; got: {:?}",
        exec.result.model_output
    );

    let host_view = std::fs::read_to_string(host_cwd.path().join("tests/agent_added_test.rs"))
        .expect("host read agent_added_test.rs");
    assert!(
        host_view.contains("agent_can_write_here"),
        "host did not see the agent's test body; got: {host_view:?}"
    );

    handle.release().await.expect("release");
    eprintln!("tests_only_write_to_tests_dir_is_allowed PASSED");
}
