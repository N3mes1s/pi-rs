//! Integration tests for the `tests_only` Cedar policy profile.
//!
//! The `tests_only` profile lets an agent read everywhere in /work
//! but only write inside `tests/` directories or to files matching
//! `*_test.rs` / `*_tests.rs`. This prevents the agent from
//! modifying implementation files while allowing it to add or edit
//! test files freely.
//!
//! Three scenarios:
//!
//! * `tests_only_read_anywhere` — reads from `src/`, `Cargo.toml`,
//!   `tests/` all succeed; verifies specific bytes in output.
//! * `tests_only_write_to_src_is_denied` — write attempts to impl
//!   files and non-test paths all fail (rc != 0); host verifies
//!   those files are byte-identical to their seeded content.
//! * `tests_only_write_to_tests_dir_is_allowed` — write of a new
//!   test file inside `tests/` succeeds; host reads back and
//!   verifies the written bytes are visible.
//!
//! Prereqs (skip cleanly when absent):
//!   PI_SANDBOX_FC_TEST=1
//!   PI_SANDBOX_CONTEXTFS_RW=1
//!   PI_SANDBOX_CEDAR_PROFILE=tests_only
//!   PI_SANDBOX_KERNEL — path to kernel image
//!   PI_SANDBOX_ROOTFS — path to rootfs image
//!   PI_SANDBOX_CFS_FS_SERVER_BIN or cfs-fs-server on PATH
//!   PI_SANDBOX_CONTEXTFS_BROKER_BIN or contextfs-broker on PATH
//!   /dev/kvm openable read/write
//!   firecracker on PATH

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

/// Resolve every prereq for the `tests_only` profile tests.
/// Returns `None` (with a `SKIP:` message printed) when anything
/// is missing.
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
        skip("PI_SANDBOX_CEDAR_PROFILE=tests_only not set — tests_only profile opt-in");
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
        skip(&format!(
            "PI_SANDBOX_KERNEL={} missing",
            kernel.display()
        ));
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
        skip(&format!(
            "PI_SANDBOX_ROOTFS={} missing",
            rootfs.display()
        ));
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

/// Stage a fake source tree in a tempdir and return the handle
/// (kept alive by the caller for the test's lifetime). Layout:
///   src/lib.rs            — "pub fn add(a: i32, b: i32) -> i32 { a + b }"
///   src/inner/mod.rs      — "pub mod math;"
///   Cargo.toml            — "[package]\nname = \"fake-crate\"\nversion = \"0.1.0\"\n"
///   tests/existing_test.rs — "#[test]\nfn it_works() { assert_eq!(2 + 2, 4); }\n"
fn staged_fake_tree() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;

    let host_cwd = tempfile::tempdir().expect("host_cwd tempdir");
    std::fs::set_permissions(host_cwd.path(), std::fs::Permissions::from_mode(0o777))
        .expect("chmod host_cwd 0777");

    let src = host_cwd.path().join("src");
    let src_inner = src.join("inner");
    let tests = host_cwd.path().join("tests");
    std::fs::create_dir_all(&src_inner).expect("mkdir src/inner");
    std::fs::create_dir_all(&tests).expect("mkdir tests");

    let write_mode = std::fs::Permissions::from_mode(0o666);

    let lib_rs = src.join("lib.rs");
    std::fs::write(&lib_rs, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n")
        .expect("write src/lib.rs");
    std::fs::set_permissions(&lib_rs, write_mode.clone()).expect("chmod src/lib.rs");

    let inner_mod = src_inner.join("mod.rs");
    std::fs::write(&inner_mod, "pub mod math;\n").expect("write src/inner/mod.rs");
    std::fs::set_permissions(&inner_mod, write_mode.clone()).expect("chmod src/inner/mod.rs");

    let cargo = host_cwd.path().join("Cargo.toml");
    std::fs::write(
        &cargo,
        "[package]\nname = \"fake-crate\"\nversion = \"0.1.0\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::set_permissions(&cargo, write_mode.clone()).expect("chmod Cargo.toml");

    let existing_test = tests.join("existing_test.rs");
    std::fs::write(
        &existing_test,
        "#[test]\nfn it_works() { assert_eq!(2 + 2, 4); }\n",
    )
    .expect("write tests/existing_test.rs");
    std::fs::set_permissions(&existing_test, write_mode).expect("chmod tests/existing_test.rs");

    host_cwd
}

/// Acquire a fresh VM rooted at `host_cwd` (RW, tests_only profile).
/// Returns the handle plus the run_root tempdir (held by caller).
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

/// Run a bash command inside the VM and return the execution result.
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

// ─────────────────────────────────────────────────────────────
// Test 1: reads from anywhere in the tree all succeed
// ─────────────────────────────────────────────────────────────
#[tokio::test]
async fn tests_only_read_anywhere() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_fake_tree();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    // Read src/lib.rs
    let r1 = run_bash(&*handle, "cat /work/src/lib.rs").await;
    assert!(
        !r1.result.is_error,
        "read src/lib.rs failed: {}",
        r1.result.model_output
    );
    assert!(
        r1.result.model_output.contains("pub fn add"),
        "src/lib.rs bytes not visible: {:?}",
        r1.result.model_output
    );

    // Read src/inner/mod.rs
    let r2 = run_bash(&*handle, "cat /work/src/inner/mod.rs").await;
    assert!(
        !r2.result.is_error,
        "read src/inner/mod.rs failed: {}",
        r2.result.model_output
    );
    assert!(
        r2.result.model_output.contains("pub mod math"),
        "src/inner/mod.rs bytes not visible: {:?}",
        r2.result.model_output
    );

    // Read Cargo.toml
    let r3 = run_bash(&*handle, "cat /work/Cargo.toml").await;
    assert!(
        !r3.result.is_error,
        "read Cargo.toml failed: {}",
        r3.result.model_output
    );
    assert!(
        r3.result.model_output.contains("fake-crate"),
        "Cargo.toml bytes not visible: {:?}",
        r3.result.model_output
    );

    // Read tests/existing_test.rs
    let r4 = run_bash(&*handle, "cat /work/tests/existing_test.rs").await;
    assert!(
        !r4.result.is_error,
        "read tests/existing_test.rs failed: {}",
        r4.result.model_output
    );
    assert!(
        r4.result.model_output.contains("it_works"),
        "tests/existing_test.rs bytes not visible: {:?}",
        r4.result.model_output
    );

    handle.release().await.expect("release");
    eprintln!("tests_only_read_anywhere PASSED");
}

// ─────────────────────────────────────────────────────────────
// Test 2: write attempts to impl files are denied
// ─────────────────────────────────────────────────────────────
#[tokio::test]
async fn tests_only_write_to_src_is_denied() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_fake_tree();

    // Capture seeded content before booting the VM
    let seed_lib =
        std::fs::read(host_cwd.path().join("src/lib.rs")).expect("read seed src/lib.rs");
    let seed_inner =
        std::fs::read(host_cwd.path().join("src/inner/mod.rs")).expect("read seed inner/mod.rs");
    let seed_cargo =
        std::fs::read(host_cwd.path().join("Cargo.toml")).expect("read seed Cargo.toml");

    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    // Attempt to overwrite src/lib.rs — must fail
    let w1 = run_bash(
        &*handle,
        "printf 'EVIL' > /work/src/lib.rs; echo RC=$?",
    )
    .await;
    // The printf redirect failing means RC!=0 OR the shell exits non-zero.
    // We check either is_error OR the output shows a non-zero rc.
    let w1_denied = w1.result.is_error
        || w1.result.model_output.contains("RC=1")
        || w1.result.model_output.contains("RC=2")
        || w1.result.model_output.contains("permission denied")
        || w1.result.model_output.to_lowercase().contains("denied")
        || w1.result.model_output.to_lowercase().contains("read-only");
    assert!(
        w1_denied,
        "write to src/lib.rs should have been denied; output: {:?}",
        w1.result.model_output
    );

    // Attempt to overwrite src/inner/mod.rs — must fail
    let w2 = run_bash(
        &*handle,
        "printf 'EVIL' > /work/src/inner/mod.rs; echo RC=$?",
    )
    .await;
    let w2_denied = w2.result.is_error
        || w2.result.model_output.contains("RC=1")
        || w2.result.model_output.contains("RC=2")
        || w2.result.model_output.to_lowercase().contains("denied")
        || w2.result.model_output.to_lowercase().contains("read-only");
    assert!(
        w2_denied,
        "write to src/inner/mod.rs should have been denied; output: {:?}",
        w2.result.model_output
    );

    // Attempt to overwrite Cargo.toml — must fail
    let w3 = run_bash(
        &*handle,
        "printf 'EVIL' > /work/Cargo.toml; echo RC=$?",
    )
    .await;
    let w3_denied = w3.result.is_error
        || w3.result.model_output.contains("RC=1")
        || w3.result.model_output.contains("RC=2")
        || w3.result.model_output.to_lowercase().contains("denied")
        || w3.result.model_output.to_lowercase().contains("read-only");
    assert!(
        w3_denied,
        "write to Cargo.toml should have been denied; output: {:?}",
        w3.result.model_output
    );

    // Attempt to create a new impl file src/new_impl.rs — must fail
    let w4 = run_bash(
        &*handle,
        "printf 'pub fn evil() {}' > /work/src/new_impl.rs; echo RC=$?",
    )
    .await;
    let w4_denied = w4.result.is_error
        || w4.result.model_output.contains("RC=1")
        || w4.result.model_output.contains("RC=2")
        || w4.result.model_output.to_lowercase().contains("denied")
        || w4.result.model_output.to_lowercase().contains("read-only");
    assert!(
        w4_denied,
        "create src/new_impl.rs should have been denied; output: {:?}",
        w4.result.model_output
    );

    handle.release().await.expect("release");

    // Host verifies: all seeded impl files are byte-identical to seeds
    let host_lib =
        std::fs::read(host_cwd.path().join("src/lib.rs")).expect("host read src/lib.rs");
    assert_eq!(
        host_lib, seed_lib,
        "src/lib.rs was modified on host — tests_only policy did not block the write"
    );

    let host_inner =
        std::fs::read(host_cwd.path().join("src/inner/mod.rs")).expect("host read inner/mod.rs");
    assert_eq!(
        host_inner, seed_inner,
        "src/inner/mod.rs was modified on host"
    );

    let host_cargo =
        std::fs::read(host_cwd.path().join("Cargo.toml")).expect("host read Cargo.toml");
    assert_eq!(host_cargo, seed_cargo, "Cargo.toml was modified on host");

    assert!(
        !host_cwd.path().join("src/new_impl.rs").exists(),
        "src/new_impl.rs was created on host — tests_only policy did not block the create"
    );

    eprintln!("tests_only_write_to_src_is_denied PASSED");
}

// ─────────────────────────────────────────────────────────────
// Test 3: write to tests/ is allowed
// ─────────────────────────────────────────────────────────────
#[tokio::test]
async fn tests_only_write_to_tests_dir_is_allowed() {
    let Some(pre) = check_prereqs() else { return };
    let host_cwd = staged_fake_tree();
    let (handle, _run_root) = acquire_vm(&pre, host_cwd.path()).await;

    let new_test_content = "#[test]\nfn agent_added() { assert!(true); }\n";

    // Write a new test file inside tests/
    let exec = run_bash(
        &*handle,
        &format!(
            "printf '{content}' > /work/tests/agent_added_test.rs && echo WRITE_OK && cat /work/tests/agent_added_test.rs",
            content = new_test_content.replace('\'', "'\\''"),
        ),
    )
    .await;
    assert!(
        !exec.result.is_error,
        "write to tests/agent_added_test.rs failed: {}",
        exec.result.model_output
    );
    assert!(
        exec.result.model_output.contains("WRITE_OK"),
        "expected WRITE_OK marker; got: {:?}",
        exec.result.model_output
    );
    assert!(
        exec.result.model_output.contains("agent_added"),
        "written content not readable in guest; got: {:?}",
        exec.result.model_output
    );

    handle.release().await.expect("release");

    // Host reads back and verifies the agent's bytes are visible
    let host_path = host_cwd.path().join("tests/agent_added_test.rs");
    assert!(
        host_path.exists(),
        "tests/agent_added_test.rs not visible on host after agent write"
    );
    let host_content = std::fs::read_to_string(&host_path)
        .expect("host read tests/agent_added_test.rs");
    assert!(
        host_content.contains("agent_added"),
        "host content of tests/agent_added_test.rs unexpected: {:?}",
        host_content
    );

    eprintln!("tests_only_write_to_tests_dir_is_allowed PASSED");
}
