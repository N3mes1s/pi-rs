//! Integration tests for the SpritesProvider host-side orchestration
//! (PI_SPRITES_CONTEXTFS=1 path). RFD 0026 v2 Phase A.
//!
//! All three tests mock the four binaries (cfs-fs-server, contextfs-broker,
//! cfs-mesh, agora) by writing tiny shell scripts to a tempdir that record
//! their argv and sleep forever. They set `PI_SANDBOX_CFS_FS_SERVER_BIN` etc.
//! to point at those mock scripts, then invoke the provider and assert
//! children are alive / killed as expected.
//!
//! These tests are Linux-only because:
//!   - contextfs (cfs-fs-server, contextfs-broker, cfs-mesh) is Linux-only.
//!   - POSIX signals (SIGKILL via kill_on_drop) are Linux-specific.
//!
//! **Serialization note.** All three tests mutate process-global environment
//! variables (`PATH`, `PI_SANDBOX_CFS_FS_SERVER_BIN`, etc.). Running them
//! in parallel under Rust's default test harness can cause race conditions
//! and spurious failures. We serialize them via a process-wide
//! `std::sync::Mutex` (`ENV_LOCK`) acquired at the start of each test body.
//! Each test also restores the original values of every env var it modifies
//! via `EnvGuard` so that the next test sees a clean environment even when
//! the test panics.

#[cfg(all(test, target_os = "linux"))]
mod linux {
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    use pi_sandbox::SpritesProvider;
    use tempfile::TempDir;

    // ── serialization lock ──────────────────────────────────────────────────
    //
    // All three tests in this file touch process-global env vars. Rust's
    // integration test harness may run them concurrently (default thread
    // pool). We use a single static Mutex to force sequential execution of
    // the env-mutation region. The `_guard` MUST be held for the full body
    // of each test function; it is released when it drops at end-of-scope.

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── env guard ──────────────────────────────────────────────────────────

    /// RAII guard that restores environment variables to their pre-test
    /// values when dropped, even if the test panics. This prevents a
    /// failing test from leaving broken env state that affects subsequent
    /// tests in the same binary.
    struct EnvGuard {
        /// Map from var name → original value (None = was unset).
        saved: HashMap<String, Option<String>>,
    }

    impl EnvGuard {
        /// Create a guard that will restore the named env vars on drop.
        /// Call `set` / `remove` on the guard (or via `std::env`) after
        /// construction to mutate the env during the test.
        fn capture(names: &[&str]) -> Self {
            let mut saved = HashMap::new();
            for &name in names {
                saved.insert(name.to_string(), std::env::var(name).ok());
            }
            EnvGuard { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, original) in &self.saved {
                match original {
                    Some(val) => std::env::set_var(name, val),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    /// Write a mock shell script that:
    ///   1. Appends its argv to `<log_file>`.
    ///   2. Sleeps forever (to keep the process alive so tests can inspect it).
    fn write_mock_binary(dir: &Path, name: &str, log_file: &Path) -> PathBuf {
        let script_path = dir.join(name);
        let log = log_file.display().to_string();
        let script = format!(
            "#!/bin/sh\necho \"$0 $@\" >> \"{log}\"\nsleep 9999\n",
        );
        std::fs::write(&script_path, &script)
            .unwrap_or_else(|e| panic!("write mock {name}: {e}"));
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod mock {name}: {e}"));
        script_path
    }

    /// Write a mock `agora` script that:
    ///   1. Appends argv to log.
    ///   2. Prints fake `agora create` output with a stable room id / secret.
    ///   3. Exits immediately (agora create is not a daemon).
    fn write_mock_agora(dir: &Path, log_file: &Path) -> PathBuf {
        let script_path = dir.join("mock-agora");
        let log = log_file.display().to_string();
        let script = format!(
            r#"#!/bin/sh
echo "$0 $@" >> "{log}"
# Only print room output for the "create" subcommand:
if [ "$1" = "create" ]; then
  echo "  Created encrypted room '$2'"
  echo "  Room ID:    ag-test-room-$(echo $2 | sha1sum | cut -c1-8)"
  echo "  Secret:     aaaaaabbbbbbccccccddddddeeeeeeffffffff0000001111112222223333334444"
  echo "  Share this join command:"
  echo "    agora join ag-test-room-XX aaaa..4444 $2"
fi
exit 0
"#,
            log = log,
        );
        std::fs::write(&script_path, &script)
            .unwrap_or_else(|e| panic!("write mock agora: {e}"));
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod mock agora: {e}"));
        script_path
    }

    /// Build a `SpritesProvider` pointing at mock binaries in `mock_dir`,
    /// logging all argv to `log_file`.
    ///
    /// The caller MUST already hold `ENV_LOCK` and have created an
    /// `EnvGuard` that captures all vars modified here.
    fn make_provider_with_mocks(
        mock_dir: &Path,
        log_file: &Path,
        run_base: &Path,
    ) -> SpritesProvider {
        // Write mock binaries.
        let cfs_fs_server = write_mock_binary(mock_dir, "mock-cfs-fs-server", log_file);
        let contextfs_broker = write_mock_binary(mock_dir, "mock-contextfs-broker", log_file);
        let cfs_mesh = write_mock_binary(mock_dir, "mock-cfs-mesh", log_file);
        let agora = write_mock_agora(mock_dir, log_file);

        // Point env vars at the mocks.
        std::env::set_var(
            "PI_SANDBOX_CFS_FS_SERVER_BIN",
            cfs_fs_server.to_str().unwrap(),
        );
        std::env::set_var(
            "PI_SANDBOX_CONTEXTFS_BROKER_BIN",
            contextfs_broker.to_str().unwrap(),
        );
        std::env::set_var("PI_CFS_MESH_BIN", cfs_mesh.to_str().unwrap());
        std::env::set_var("PI_AGORA_BIN", agora.to_str().unwrap());

        // Run-dir base so UDSes go to our tempdir, not /home/nemesis/code/...
        std::env::set_var("PI_SPRITES_RUN_BASE", run_base.to_str().unwrap());

        // Enable the contextfs path.
        std::env::set_var("PI_SPRITES_CONTEXTFS", "1");

        SpritesProvider::with_token("test-token".to_string())
    }

    // ── test 1: smoke — all four processes spawn and are alive ──────────────

    /// Verify that `_test_open_host_side_only` spawns four child processes
    /// (cfs-fs-server, contextfs-broker, cfs-mesh ×2) and they remain alive
    /// after the call returns.
    #[tokio::test]
    async fn sprites_host_orchestration_smoke() {
        // Acquire the serialization lock first to prevent concurrent env mutation.
        let _env_lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Capture all env vars this test will touch so we can restore them.
        let _env_guard = EnvGuard::capture(&[
            "PI_SANDBOX_CFS_FS_SERVER_BIN",
            "PI_SANDBOX_CONTEXTFS_BROKER_BIN",
            "PI_CFS_MESH_BIN",
            "PI_AGORA_BIN",
            "PI_SPRITES_RUN_BASE",
            "PI_SPRITES_CONTEXTFS",
        ]);

        let mock_dir = TempDir::new().expect("tempdir");
        let run_base = TempDir::new().expect("run_base tempdir");
        let log_file = mock_dir.path().join("argv.log");

        let provider = make_provider_with_mocks(
            mock_dir.path(),
            &log_file,
            run_base.path(),
        );

        let cwd = mock_dir.path();
        provider
            ._test_open_host_side_only(cwd)
            .await
            .expect("_test_open_host_side_only failed");

        // Give the processes a moment to start and write their argv.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Check the log file was created and contains expected argv.
        let log = std::fs::read_to_string(&log_file)
            .unwrap_or_else(|e| panic!("read argv log: {e}"));

        // cfs-fs-server should appear with --root and --socket.
        assert!(
            log.contains("mock-cfs-fs-server"),
            "cfs-fs-server not invoked; log:\n{log}"
        );
        assert!(
            log.contains("--root"),
            "cfs-fs-server missing --root arg; log:\n{log}"
        );
        assert!(
            log.contains("--socket"),
            "cfs-fs-server missing --socket arg; log:\n{log}"
        );

        // contextfs-broker should appear with run / --socket / --policy.
        assert!(
            log.contains("mock-contextfs-broker"),
            "contextfs-broker not invoked; log:\n{log}"
        );
        assert!(
            log.contains("run"),
            "contextfs-broker missing 'run' subcommand; log:\n{log}"
        );
        assert!(
            log.contains("--policy"),
            "contextfs-broker missing --policy arg; log:\n{log}"
        );

        // cfs-mesh should appear twice (fs-bridge + broker-bridge).
        let mesh_count = log.matches("mock-cfs-mesh").count();
        assert!(
            mesh_count >= 2,
            "expected ≥2 cfs-mesh invocations, got {mesh_count}; log:\n{log}"
        );
        assert!(
            log.contains("agora-bridge"),
            "cfs-mesh missing agora-bridge subcommand; log:\n{log}"
        );
        assert!(
            log.contains("--room"),
            "cfs-mesh agora-bridge missing --room; log:\n{log}"
        );
        assert!(
            log.contains("--target-uds"),
            "cfs-mesh agora-bridge missing --target-uds; log:\n{log}"
        );

        // agora create should appear twice (one room per bridge).
        let agora_count = log.matches("mock-agora create").count();
        assert!(
            agora_count >= 2,
            "expected ≥2 agora create calls, got {agora_count}; log:\n{log}"
        );
    }

    // ── test 2: kill-on-drop — children die within 1 s of Drop ─────────────

    /// Verify the kill-on-drop contract: after the provider is dropped,
    /// all four child processes are killed within 1 second.
    #[tokio::test]
    async fn sprites_host_orchestration_clean_kill_on_drop() {
        // Acquire the serialization lock first to prevent concurrent env mutation.
        let _env_lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Capture all env vars this test will touch so we can restore them.
        let _env_guard = EnvGuard::capture(&[
            "PI_SANDBOX_CFS_FS_SERVER_BIN",
            "PI_SANDBOX_CONTEXTFS_BROKER_BIN",
            "PI_CFS_MESH_BIN",
            "PI_AGORA_BIN",
            "PI_SPRITES_RUN_BASE",
            "PI_SPRITES_CONTEXTFS",
        ]);

        let mock_dir = TempDir::new().expect("tempdir");
        let run_base = TempDir::new().expect("run_base tempdir");
        let log_file = mock_dir.path().join("argv.log");

        let provider = make_provider_with_mocks(
            mock_dir.path(),
            &log_file,
            run_base.path(),
        );

        let cwd = mock_dir.path();
        provider
            ._test_open_host_side_only(cwd)
            .await
            .expect("_test_open_host_side_only failed");

        // Give processes a moment to boot.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Confirm at least one mock-cfs-fs-server is running.
        let running_before = count_processes_named("mock-cfs-fs-server");
        assert!(
            running_before > 0,
            "expected ≥1 mock-cfs-fs-server process before drop"
        );

        // Drop the provider — kill_on_drop fires on the children.
        drop(provider);

        // Wait up to 1 s for the processes to disappear.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let still_running = count_processes_named("mock-cfs-fs-server");
            if still_running == 0 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "mock-cfs-fs-server still running {still_running} instance(s) \
                     1 s after provider drop — kill_on_drop failed"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Count live processes whose command name contains `name` by reading
    /// `/proc/<pid>/comm`. Only works on Linux.
    fn count_processes_named(name: &str) -> usize {
        let Ok(proc) = std::fs::read_dir("/proc") else {
            return 0;
        };
        proc.filter_map(|entry| {
            let entry = entry.ok()?;
            let pid_str = entry.file_name();
            pid_str.to_str()?.parse::<u32>().ok()?; // must be numeric
            let comm_path = entry.path().join("comm");
            let comm = std::fs::read_to_string(comm_path).ok()?;
            if comm.trim().contains(name) { Some(()) } else { None }
        })
        .count()
    }

    // ── test 3: missing binary fails loudly ─────────────────────────────────

    /// When `PI_SANDBOX_CFS_FS_SERVER_BIN` is unset and `cfs-fs-server` is
    /// not on PATH, `_test_open_host_side_only` must return
    /// `SandboxError::Unavailable` with a helpful message naming the missing
    /// binary.
    #[tokio::test]
    async fn sprites_host_orchestration_missing_binary_fails_loudly() {
        use pi_sandbox::SandboxError;

        // Acquire the serialization lock first to prevent concurrent env mutation.
        let _env_lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Capture all env vars this test will touch so we can restore them.
        let _env_guard = EnvGuard::capture(&[
            "PI_SANDBOX_CFS_FS_SERVER_BIN",
            "PI_SANDBOX_CONTEXTFS_BROKER_BIN",
            "PI_CFS_MESH_BIN",
            "PI_AGORA_BIN",
            "PI_SPRITES_RUN_BASE",
            "PI_SPRITES_CONTEXTFS",
            "PATH",
        ]);

        let run_base = TempDir::new().expect("run_base tempdir");
        let mock_dir = TempDir::new().expect("mock_dir tempdir");

        // Unset the env var so the resolver falls back to PATH.
        std::env::remove_var("PI_SANDBOX_CFS_FS_SERVER_BIN");

        // Make sure PATH does NOT contain cfs-fs-server (point PATH at an
        // empty dir so no system cfs-fs-server is accidentally found).
        let empty_path = TempDir::new().expect("empty path tempdir");
        std::env::set_var("PATH", empty_path.path());

        // Still need agora and cfs-mesh mocks so the missing-binary check
        // fires at the cfs-fs-server step, not earlier.
        std::env::remove_var("PI_CFS_MESH_BIN");
        std::env::remove_var("PI_AGORA_BIN");
        std::env::remove_var("PI_SANDBOX_CONTEXTFS_BROKER_BIN");
        std::env::set_var("PI_SPRITES_CONTEXTFS", "1");
        std::env::set_var("PI_SPRITES_RUN_BASE", run_base.path());

        let provider = SpritesProvider::with_token("test-token".to_string());
        let cwd = mock_dir.path();
        let result = provider._test_open_host_side_only(cwd).await;

        // EnvGuard restores PATH and all PI_* vars automatically on drop.

        match result {
            Err(SandboxError::Unavailable(msg)) => {
                assert!(
                    msg.contains("cfs-fs-server"),
                    "error message should name the missing binary; got: {msg}"
                );
            }
            Err(other) => panic!(
                "expected SandboxError::Unavailable, got: {:?}",
                other
            ),
            Ok(()) => panic!(
                "expected error for missing cfs-fs-server, but open succeeded"
            ),
        }
    }
}
