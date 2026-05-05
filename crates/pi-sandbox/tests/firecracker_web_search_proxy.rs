//! End-to-end: guest agent calls `web_search`, request proxies via
//! vsock to the host's `WebSearchTool`, response comes back. Per
//! RFD 0023 §"web_search via vsock proxy".
//!
//! Without a real upstream API key the host's `WebSearchTool` returns
//! an "missing API key in env" error. That's enough to prove the
//! whole proxy pipeline works — guest worker recognizes web_search,
//! vsock-5003 connection succeeds, host listener accepts, dispatches
//! to `WebSearchTool`, ships error back, guest surfaces it as a
//! ToolResponse with `is_error=true`.
//!
//! Gated on `PI_SANDBOX_FC_TEST=1` (same gate as `firecracker_smoke`).

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use pi_sandbox::microvm::firecracker::{FirecrackerConfig, FirecrackerLauncher};
use pi_sandbox::microvm::launcher::MicroVmLauncher;
use pi_sandbox::microvm::{CallLimits, NetworkPolicy, RootfsVersion, VmCeiling, VmSpec};
use pi_tools::ToolContext;
use serde_json::json;

fn skip(reason: &str) {
    eprintln!("SKIP: {reason}");
}

macro_rules! require_env {
    ($var:expr) => {
        match std::env::var($var) {
            Ok(v) if !v.is_empty() => v,
            _ => {
                skip(&format!("env var {} not set — skipping web_search proxy test", $var));
                return;
            }
        }
    };
}

#[tokio::test]
async fn web_search_proxies_via_vsock_to_host() {
    require_env!("PI_SANDBOX_FC_TEST");
    if which::which("firecracker").is_err() {
        skip("firecracker binary not on PATH");
        return;
    }
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_err()
    {
        skip("/dev/kvm not openable RW");
        return;
    }
    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_KERNEL not set"),
    };
    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_ROOTFS not set"),
    };
    if !kernel_path.exists() || !rootfs_path.exists() {
        return skip("kernel or rootfs path missing");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tempfile::tempdir().expect("work");
    let cfg = FirecrackerConfig {
        kernel_path: Some(kernel_path),
        rootfs_path: Some(rootfs_path),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(cfg);
    // `web_search` proxy listener only binds when the operator's
    // policy is `Allow` — vsock is otherwise a parallel channel
    // that would silently bypass `Deny`. We use Allow here with an
    // empty allowlist (web_search doesn't go through eth0; the
    // allowlist applies to eth0 only).
    let spec = VmSpec {
        host_cwd: work.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Allow {
            tap_name: "tap-pi0".into(),
            guest_ip_cidr: "172.16.0.2/30".into(),
            guest_gateway: "172.16.0.1".into(),
            guest_dns: vec!["1.1.1.1".into()],
            guest_mac: None,
            egress_allowlist: vec![], // no eth0 egress; vsock-only
        },
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    let h = launcher.acquire(&spec).await.expect("acquire");

    // Detect whether the host has a search-provider API key. If yes
    // we assert SUCCESS (the proxy round-trip really worked and the
    // upstream API answered with results). If no we assert the
    // host's "missing API key" error surfaces — also proves the
    // round-trip but only the in-process leg.
    let key_present = std::env::var("PARALLEL_API_KEY").is_ok()
        || std::env::var("EXA_API_KEY").is_ok()
        || std::env::var("BRAVE_API_KEY").is_ok()
        || std::env::var("JINA_API_KEY").is_ok()
        || std::env::var("PERPLEXITY_API_KEY").is_ok();

    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(30),
                ..Default::default()
            },
            "web_search",
            &json!({"query": "rust programming language"}),
        )
        .await
        .expect("web_search tool dispatch");

    eprintln!(
        "web_search response (key_present={}): is_error={}\nbody:\n{}",
        key_present,
        r.result.is_error,
        // Truncate for log; full body still asserted below.
        r.result.model_output.chars().take(800).collect::<String>()
    );

    if key_present {
        assert!(
            !r.result.is_error,
            "web_search proxy returned error despite a search key in host env: {}",
            r.result.model_output
        );
        let body = r.result.model_output.to_lowercase();
        // The default `WebSearchTool` formats results with a header
        // like "web_search via <provider> for: <query>" plus per-
        // result lines. A non-empty body containing the query is a
        // strong signal that real upstream content came back.
        assert!(
            body.contains("rust") && body.len() > 200,
            "web_search proxy returned a body that doesn't look like real results (len={}): {}",
            body.len(),
            r.result.model_output
        );
        assert!(
            body.contains("web_search via"),
            "web_search proxy body missing the WebSearchTool header — did the proxy maybe stub the response? body: {}",
            r.result.model_output
        );
    } else {
        assert!(
            r.result.is_error,
            "expected is_error=true (no key in host env); got: {}",
            r.result.model_output
        );
        let body = r.result.model_output.to_lowercase();
        assert!(
            body.contains("api key") || body.contains("websearchtool") || body.contains("missing"),
            "expected host's WebSearchTool error to surface; got: {}",
            r.result.model_output
        );
    }

    h.release().await.expect("release");
}

/// Demonstrate per-call latency on the warm path: acquire ONE VM,
/// fire two `web_search` calls back-to-back on the same handle, log
/// the wall time of each. Call 1 includes any per-call setup that
/// the worker does on first dispatch; call 2 is a pure proxy +
/// upstream API round-trip — it's what the steady-state cost looks
/// like.
#[tokio::test]
async fn web_search_two_calls_one_vm_latency() {
    require_env!("PI_SANDBOX_FC_TEST");
    if which::which("firecracker").is_err() {
        skip("firecracker binary not on PATH");
        return;
    }
    if std::env::var("PARALLEL_API_KEY").is_err()
        && std::env::var("EXA_API_KEY").is_err()
        && std::env::var("BRAVE_API_KEY").is_err()
        && std::env::var("JINA_API_KEY").is_err()
        && std::env::var("PERPLEXITY_API_KEY").is_err()
    {
        return skip("no search-provider API key in host env — skipping live latency demo");
    }

    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_KERNEL not set"),
    };
    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_ROOTFS not set"),
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tempfile::tempdir().expect("work");
    let cfg = FirecrackerConfig {
        kernel_path: Some(kernel_path),
        rootfs_path: Some(rootfs_path),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(cfg);
    let spec = VmSpec {
        host_cwd: work.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Allow {
            tap_name: "tap-pi0".into(),
            guest_ip_cidr: "172.16.0.2/30".into(),
            guest_gateway: "172.16.0.1".into(),
            guest_dns: vec!["1.1.1.1".into()],
            guest_mac: None,
            egress_allowlist: vec![],
        },
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    let acquire_started = std::time::Instant::now();
    let h = launcher.acquire(&spec).await.expect("acquire");
    let acquire_ms = acquire_started.elapsed().as_millis();
    eprintln!("acquire (cold-boot path): {acquire_ms} ms");

    let queries = ["rust programming language", "linux kernel scheduler"];
    for (i, q) in queries.iter().enumerate() {
        let started = std::time::Instant::now();
        let r = h
            .execute(
                &ToolContext::default(),
                &CallLimits {
                    wall_timeout: Duration::from_secs(30),
                    ..Default::default()
                },
                "web_search",
                &json!({"query": q}),
            )
            .await
            .expect("web_search dispatch");
        let dt_ms = started.elapsed().as_millis();
        let body_len = r.result.model_output.len();
        eprintln!(
            "call {} (q={:?}): {dt_ms} ms  is_error={}  body_len={body_len}",
            i + 1,
            q,
            r.result.is_error
        );
        assert!(
            !r.result.is_error,
            "call {} failed: {}",
            i + 1,
            r.result.model_output
        );
        assert!(
            body_len > 200,
            "call {} body too small ({}): {}",
            i + 1,
            body_len,
            r.result.model_output
        );
    }

    h.release().await.expect("release");
}

/// `NetworkPolicy::Deny` must block the `web_search` proxy too —
/// otherwise the operator's "no network" intent is silently bypassed
/// by any vsock-proxied tool. The listener is never bound, so the
/// guest's vsock_connect to (HOST_CID, 5003) returns
/// connection-refused; the worker translates that into a clean
/// `is_error=true` ToolResponse mentioning "vsock" / "io".
#[tokio::test]
async fn web_search_blocked_under_network_policy_deny() {
    require_env!("PI_SANDBOX_FC_TEST");
    if which::which("firecracker").is_err() {
        skip("firecracker binary not on PATH");
        return;
    }
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_err()
    {
        skip("/dev/kvm not openable RW");
        return;
    }
    let kernel_path = match std::env::var("PI_SANDBOX_KERNEL") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_KERNEL not set"),
    };
    let rootfs_path = match std::env::var("PI_SANDBOX_ROOTFS") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return skip("PI_SANDBOX_ROOTFS not set"),
    };
    if !kernel_path.exists() || !rootfs_path.exists() {
        return skip("kernel or rootfs path missing");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tempfile::tempdir().expect("work");
    let cfg = FirecrackerConfig {
        kernel_path: Some(kernel_path),
        rootfs_path: Some(rootfs_path),
        run_dir: tmp.path().join("run"),
        pool_size: 1,
        ..Default::default()
    };
    let launcher = FirecrackerLauncher::new(cfg);
    let spec = VmSpec {
        host_cwd: work.path().to_path_buf(),
        host_cwd_writable: true,
        env: Default::default(),
        network_policy: NetworkPolicy::Deny,
        vm_ceiling: VmCeiling::default(),
        rootfs_version: RootfsVersion::current(),
    };

    let h = launcher.acquire(&spec).await.expect("acquire");
    let r = h
        .execute(
            &ToolContext::default(),
            &CallLimits {
                wall_timeout: Duration::from_secs(15),
                ..Default::default()
            },
            "web_search",
            &json!({"query": "anything"}),
        )
        .await
        .expect("web_search dispatch under Deny");

    eprintln!(
        "Deny-mode web_search response: is_error={} body={:?}",
        r.result.is_error, r.result.model_output
    );
    assert!(
        r.result.is_error,
        "web_search should fail under NetworkPolicy::Deny; got: {}",
        r.result.model_output
    );
    let body = r.result.model_output.to_lowercase();
    assert!(
        body.contains("vsock") || body.contains("connection") || body.contains("refused") || body.contains("io"),
        "expected a vsock/connection-refused error under Deny; got: {}",
        r.result.model_output
    );
    // Crucially, the error must NOT look like a successful host
    // dispatch (which would mention `WebSearchTool` or `api key`):
    assert!(
        !body.contains("websearchtool") && !body.contains("api key"),
        "Deny-mode web_search reached the host listener — policy bypass!  Body: {}",
        r.result.model_output
    );

    h.release().await.expect("release");
}
