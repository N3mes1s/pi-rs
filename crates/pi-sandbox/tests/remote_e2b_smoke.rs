//! Live E2B integration smoke test (gated on `E2B_API_KEY` + `PI_SANDBOX_WORKER_BIN`).
//!
//! # Prerequisites for running this test
//!
//! ```sh
//! export E2B_API_KEY=e2b_...              # live E2B API key
//! export PI_SANDBOX_WORKER_BIN=/path/to/pi-sandbox-worker  # musl binary
//! ```
//!
//! When either variable is absent the test prints a skip message and returns
//! with exit 0. This means `cargo test -p pi-sandbox` without the credentials
//! always passes — critical for CI hygiene on hosts without an E2B account.
//!
//! # What this test proves
//!
//! 1. `E2bProvider::from_env()` constructs without panic.
//! 2. The provider is a valid `SandboxProvider` (name == "e2b").
//! 3. SmartSync uploads the host cwd to `/work` in the remote sandbox:
//!    a host file staged in the tempdir is visible inside the sandbox.
//! 4. `execute_tool("bash", {"command": "ls /work && cat /work/host_seed.txt"})`
//!    returns exit_status 0 and stdout containing the seed payload.
//! 5. `cleanup()` completes without panic, and is always attempted even when
//!    earlier steps fail or the wall-clock budget expires.
//!
//! # Wall-time
//!
//! Cold start (sandbox create + 7 MB worker upload + file sync) typically
//! takes 10–20 s on a warm E2B account.  The test times out after 30 s and
//! fails loudly if exceeded.
#![cfg(target_os = "linux")]

use pi_sandbox::{E2bProvider, SandboxProvider};
use pi_tools::ToolContext;
use serde_json::json;

/// Seed content written to the host tempdir and expected inside the sandbox.
const SEED_CONTENT: &str = "e2b-roundtrip-marker";
/// Filename of the seed file.
const SEED_FILE: &str = "host_seed.txt";

#[tokio::test(flavor = "multi_thread")]
async fn remote_e2b_roundtrip() {
    // --- skip contract ---
    let api_key = match std::env::var("E2B_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!("SKIP: E2B_API_KEY not set");
            return;
        }
    };
    let worker_bin = match std::env::var("PI_SANDBOX_WORKER_BIN") {
        Ok(p) if !p.trim().is_empty() => p,
        _ => {
            eprintln!("SKIP: PI_SANDBOX_WORKER_BIN not set");
            return;
        }
    };
    // Verify the binary actually exists so the error is friendly.
    if !std::path::Path::new(&worker_bin).exists() {
        eprintln!("SKIP: PI_SANDBOX_WORKER_BIN={worker_bin} does not exist");
        return;
    }

    // 1. Construct E2bProvider from the env (E2B_API_KEY is already set).
    //    from_env() must not panic. Provider is created HERE, outside the
    //    timed future, so cleanup can always be attempted after the timeout
    //    or assertion fires.
    let provider = E2bProvider::from_env()
        .expect("E2bProvider::from_env() must succeed when E2B_API_KEY is set");

    assert_eq!(provider.name(), "e2b", "provider slug must be 'e2b'");

    // 2. Build a ToolContext backed by a real tempdir.
    let work_dir = tempfile::tempdir().expect("create tempdir");
    let mut ctx = ToolContext::default();
    ctx.cwd = work_dir.path().to_path_buf();

    // 3. Stage host_seed.txt in the tempdir (SmartSync will upload it to /work).
    let seed_path = work_dir.path().join(SEED_FILE);
    std::fs::write(&seed_path, SEED_CONTENT)
        .unwrap_or_else(|e| panic!("write seed file: {e}"));

    // 4. Execute the bash tool inside a 30-second budget.
    //    The provider lives OUTSIDE the timeout so cleanup is reachable
    //    even if the timeout fires (the future is dropped, not the provider).
    let exec_result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        provider.execute_tool(
            &ctx,
            "bash",
            &json!({"command": format!("ls /work && cat /work/{SEED_FILE}")}),
        ),
    )
    .await;

    // Best-effort cleanup: always attempt this, regardless of how the
    // execute step ended (timeout, error, or success).  Failures here are
    // logged but do not mask the primary failure.
    let cleanup_result = provider.cleanup().await;
    if let Err(ref e) = cleanup_result {
        eprintln!("[e2b-smoke] WARN: cleanup() returned an error: {e}");
    }

    // Now evaluate the execute result and assert (after cleanup is done).
    let exec = match exec_result {
        Err(_elapsed) => {
            panic!(
                "remote_e2b_roundtrip exceeded the 30-second wall-time budget; \
                 something is wrong with the E2B session (cold start + upload normally \
                 completes in < 20 s on a warm account)"
            );
        }
        Ok(Err(e)) => {
            panic!("execute_tool failed: {e}");
        }
        Ok(Ok(exec)) => exec,
    };

    // Report cost so operators can see what one round-trip costs.
    eprintln!(
        "[e2b-smoke] round_trip_ms={:?}  cost_usd={:?}",
        exec.round_trip_ms, exec.cost_usd
    );
    eprintln!("[e2b-smoke] stdout: {}", exec.stdout.trim_end());
    if !exec.stderr.is_empty() {
        eprintln!("[e2b-smoke] stderr: {}", exec.stderr.trim_end());
    }

    // 5. Assert success.
    assert_eq!(
        exec.exit_status, 0,
        "expected exit_status 0; stdout={:?} stderr={:?}",
        exec.stdout, exec.stderr
    );
    assert!(
        exec.stdout.contains(SEED_CONTENT),
        "expected stdout to contain {:?} (the seed payload uploaded via SmartSync), \
         but got: {:?}",
        SEED_CONTENT,
        exec.stdout
    );

    eprintln!(
        "[e2b-smoke] ✓ round-trip OK  api_key={}...",
        &api_key[..4.min(api_key.len())]
    );
}
