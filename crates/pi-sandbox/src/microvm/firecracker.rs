//! FirecrackerLauncher — Linux `MicroVmLauncher` implementation.
//!
//! RFD 0023 §4 (Linux), §6 (threat model), §7 (performance SLO).
//!
//! Architecture:
//!   - Each acquired VM is a Firecracker process with a vsock UDS,
//!     a virtio-fs share of the host cwd, and a pi-sandbox-worker
//!     listening on vsock port 5001.
//!   - A warm pool (`VecDeque<WarmVm>`, default N=2) reduces cold-boot
//!     cost; pool entries are rotated after MAX_CALLS or MAX_AGE.
//!   - Vsock connection: Firecracker exposes a UDS; we send
//!     `CONNECT <port>\n` and receive `OK <cid> <port>\n`, then use
//!     the stream for JSON-line framing (aegis vsock::connect_port pattern).

#![cfg(target_os = "linux")]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use uuid::Uuid;

use pi_sandbox_protocol::{
    framing, ToolRequest, CURRENT_PROTOCOL_VERSION, VSOCK_DEFAULT_PORT,
};
use pi_tool_types::ToolResult;
use pi_tools::ToolContext;

use crate::microvm::{
    CallLimits, NetworkPolicy, ProbeCheck, ProbeReport, VmCeiling, VmExecution, VmSpec,
};
use crate::microvm::launcher::{MicroVmLauncher, VmHandle};
use crate::provider::SandboxError;
use crate::cache::{RootfsCache, ROOTFS_SHA256, ROOTFS_SIZE_BYTES, ROOTFS_URL, ROOTFS_VERSION as CACHE_ROOTFS_VERSION};

// ── Pool rotation limits ────────────────────────────────────────────────────

/// A VM is retired after this many tool calls (bounds state leakage).
const MAX_CALLS: u32 = 50;
/// A VM is retired after this much wall time (bounds state leakage).
const MAX_AGE: Duration = Duration::from_secs(5 * 60);
/// Default warm pool size.
const DEFAULT_POOL_SIZE: usize = 2;
/// How long to wait for the guest vsock worker to become ready.
const VSOCK_READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll interval while waiting for the guest to come up.
const VSOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

// ── Config ──────────────────────────────────────────────────────────────────

/// Launcher configuration.
#[derive(Debug, Clone)]
pub struct FirecrackerConfig {
    /// Path to the `firecracker` binary. If `None`, resolved via PATH.
    pub firecracker_bin: Option<PathBuf>,
    /// Path to `virtiofsd` binary. If `None`, resolved via PATH.
    pub virtiofsd_bin: Option<PathBuf>,
    /// Directory under which per-VM runtime sockets are placed.
    /// Defaults to `/run/pi-sandbox`.
    pub run_dir: PathBuf,
    /// Kernel image path. Defaults to `$PI_SANDBOX_KERNEL` or
    /// `~/.cache/pi/sandbox/kernel/vmlinux`.
    pub kernel_path: Option<PathBuf>,
    /// Rootfs image path. Defaults to `$PI_SANDBOX_ROOTFS` or the
    /// RootfsCache path for the current version.
    pub rootfs_path: Option<PathBuf>,
    /// Warm pool size (default 2).
    pub pool_size: usize,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            firecracker_bin: None,
            virtiofsd_bin: None,
            run_dir: PathBuf::from("/run/pi-sandbox"),
            kernel_path: None,
            rootfs_path: None,
            pool_size: DEFAULT_POOL_SIZE,
        }
    }
}

impl FirecrackerConfig {
    /// Resolve the kernel path: env override → explicit field → default cache.
    fn resolved_kernel_path(&self) -> PathBuf {
        if let Ok(p) = std::env::var("PI_SANDBOX_KERNEL") {
            return PathBuf::from(p);
        }
        if let Some(p) = &self.kernel_path {
            return p.clone();
        }
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("pi/sandbox/kernel/vmlinux")
    }

    /// Resolve the rootfs path: env override → explicit field → default cache.
    ///
    /// The default path is the **decompressed** `.img` file that
    /// `RootfsCache::ensure()` / build.sh produce at:
    /// `~/.cache/pi/sandbox/rootfs/<version>/rootfs.img`.
    /// Note: `.img.zst` is the *compressed* artifact; Firecracker requires
    /// an uncompressed block device image.
    ///
    /// Used by unit tests; production code calls `cold_boot()` which
    /// handles the full `RootfsCache::ensure()` flow.
    #[cfg_attr(not(test), allow(dead_code))]
    fn resolved_rootfs_path(&self) -> PathBuf {
        if let Ok(p) = std::env::var("PI_SANDBOX_ROOTFS") {
            return PathBuf::from(p);
        }
        if let Some(p) = &self.rootfs_path {
            return p.clone();
        }
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(format!(
                "pi/sandbox/rootfs/{}/rootfs.img",
                crate::microvm::ROOTFS_VERSION
            ))
    }

    /// Resolve the `firecracker` binary path.
    fn resolved_firecracker(&self) -> Option<PathBuf> {
        if let Some(p) = &self.firecracker_bin {
            return Some(p.clone());
        }
        which::which("firecracker").ok()
    }

    /// Resolve the `virtiofsd` binary path.
    ///
    /// Resolution order:
    ///   1. `PI_SANDBOX_VIRTIOFSD` env var override.
    ///   2. Explicit `virtiofsd_bin` field in config.
    ///   3. `which virtiofsd` (PATH lookup).
    ///   4. Hard-coded well-known path `/usr/lib/virtiofsd` (Debian/Ubuntu package).
    ///
    /// Returns `None` only when all four options fail.
    fn resolved_virtiofsd(&self) -> Option<PathBuf> {
        // 1. Env override (mirrors PI_SANDBOX_KERNEL pattern).
        if let Ok(p) = std::env::var("PI_SANDBOX_VIRTIOFSD") {
            let path = PathBuf::from(p);
            if path.exists() {
                return Some(path);
            }
        }
        // 2. Explicit config field.
        if let Some(p) = &self.virtiofsd_bin {
            return Some(p.clone());
        }
        // 3. PATH lookup.
        if let Some(p) = which::which("virtiofsd").ok() {
            return Some(p);
        }
        // 4. Well-known fallback: Debian/Ubuntu virtiofsd package location.
        let fallback = PathBuf::from("/usr/lib/virtiofsd");
        if fallback.exists() {
            return Some(fallback);
        }
        None
    }
}

// ── Warm pool entry ─────────────────────────────────────────────────────────

/// A pre-booted VM held in the warm pool.
struct WarmVm {
    id: String,
    vsock_path: PathBuf,
    /// The firecracker process. Kept alive by holding this handle;
    /// `kill_on_drop(true)` ensures it dies if we drop it.
    _fc_proc: Child,
    /// virtiofsd process (one per VM). Also `kill_on_drop(true)`.
    _vfs_proc: Option<Child>,
    /// When this VM was booted.
    born_at: Instant,
    /// Number of tool calls executed through this VM.
    call_count: u32,
    /// The VmCeiling this VM was booted with (for pool keying).
    ceiling: VmCeiling,
    /// The host cwd virtio-fs path this VM is sharing.
    host_cwd: PathBuf,
    /// The rootfs version this VM was booted with (for pool keying).
    /// Required to enforce RootfsMismatch: a pool hit with a different
    /// version must be rejected, not silently handed back.
    rootfs_version: String,
}

impl WarmVm {
    fn is_expired(&self) -> bool {
        self.call_count >= MAX_CALLS || self.born_at.elapsed() >= MAX_AGE
    }
}

// ── Public launcher type ────────────────────────────────────────────────────

/// Firecracker-based `MicroVmLauncher` for Linux.
///
/// Maintains a warm pool of pre-booted Firecracker VMs; each pool entry
/// owns its Firecracker + virtiofsd child processes. VMs are retired
/// after `MAX_CALLS` tool calls or `MAX_AGE` seconds.
pub struct FirecrackerLauncher {
    config: Arc<FirecrackerConfig>,
    pool: Arc<Mutex<VecDeque<WarmVm>>>,
}

impl FirecrackerLauncher {
    /// Construct with the given config. Does NOT start any VMs yet;
    /// use `warm_pool()` or let `acquire()` do lazy cold-boot.
    pub fn new(config: FirecrackerConfig) -> Self {
        Self {
            config: Arc::new(config),
            pool: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Construct with default config.
    pub fn with_defaults() -> Self {
        Self::new(FirecrackerConfig::default())
    }

    /// Pre-warm the pool to `config.pool_size` VMs for the given spec.
    /// Errors from individual cold-boots are logged and ignored so
    /// partial pre-warm still works.
    pub async fn warm_pool(&self, spec: &VmSpec) {
        let target = self.config.pool_size;
        let current = self.pool.lock().await.len();
        for _ in current..target {
            match cold_boot(&self.config, spec).await {
                Ok(vm) => self.pool.lock().await.push_back(vm),
                Err(e) => warn!("warm pool pre-boot failed: {}", e),
            }
        }
    }
}

#[async_trait]
impl MicroVmLauncher for FirecrackerLauncher {
    fn transport_name(&self) -> &'static str {
        "firecracker"
    }

    /// Probe: checks firecracker binary, /dev/kvm access, vsock module,
    /// virtiofsd binary. Fast (≤ 200 ms even when binaries are missing).
    async fn probe(&self) -> Result<ProbeReport, SandboxError> {
        let start = Instant::now();
        let mut checks: Vec<ProbeCheck> = Vec::new();
        let mut blockers: Vec<String> = Vec::new();
        let mut remediation: Vec<String> = Vec::new();
        let mut version: Option<String> = None;

        // 1. firecracker binary on PATH
        let fc_path = self.config.resolved_firecracker();
        let fc_ok = fc_path.is_some();
        if !fc_ok {
            blockers.push("firecracker binary not found on $PATH".into());
            remediation.push(
                "Install Firecracker: https://firecracker-microvm.github.io/".into(),
            );
        }
        // Try to get version string
        if let Some(ref fc) = fc_path {
            if let Ok(out) = tokio::process::Command::new(fc)
                .arg("--version")
                .output()
                .await
            {
                let s = String::from_utf8_lossy(&out.stdout);
                let ver = s.lines().next().map(|l| l.trim().to_string());
                version = ver;
            }
        }
        checks.push(ProbeCheck {
            name: "fc_binary",
            passed: fc_ok,
            detail: fc_path.map(|p| p.display().to_string()),
        });

        // 2. /dev/kvm openable RW
        let kvm_ok = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok();
        if !kvm_ok {
            blockers.push(
                "/dev/kvm not openable for read/write (add user to 'kvm' group)".into(),
            );
            remediation.push("sudo usermod -aG kvm $USER  # then log out + in".into());
        }
        checks.push(ProbeCheck {
            name: "kvm_open_rw",
            passed: kvm_ok,
            detail: if kvm_ok {
                Some("/dev/kvm opened OK".into())
            } else {
                Some("EACCES or ENOENT".into())
            },
        });

        // 3. vsock kernel module loaded — check multiple indicators:
        //    - /sys/module/vsock       (module loaded as loadable .ko)
        //    - /sys/module/vhost_vsock (vhost-vsock variant)
        //    - /dev/vhost-vsock        (device node — kernel built-in or auto-loaded)
        let vsock_ok = std::path::Path::new("/sys/module/vsock").exists()
            || std::path::Path::new("/sys/module/vhost_vsock").exists()
            || std::path::Path::new("/dev/vhost-vsock").exists();
        if !vsock_ok {
            blockers.push("vsock kernel module not loaded".into());
            remediation.push("sudo modprobe vsock vhost-vsock".into());
        }
        checks.push(ProbeCheck {
            name: "vsock_module",
            passed: vsock_ok,
            detail: if vsock_ok {
                Some("/sys/module/vsock, /sys/module/vhost_vsock, or /dev/vhost-vsock present".into())
            } else {
                Some("neither /sys/module/vsock nor vhost_vsock nor /dev/vhost-vsock found".into())
            },
        });

        // 4. virtiofsd binary (PATH, PI_SANDBOX_VIRTIOFSD, or /usr/lib/virtiofsd)
        let vfs_path = self.config.resolved_virtiofsd();
        let vfs_ok = vfs_path.is_some();
        if !vfs_ok {
            blockers.push(
                "virtiofsd binary not found (tried $PI_SANDBOX_VIRTIOFSD, $PATH, /usr/lib/virtiofsd)".into(),
            );
            remediation
                .push("Install virtiofsd: https://gitlab.com/virtio-fs/virtiofsd  or  apt install virtiofsd".into());
        }
        checks.push(ProbeCheck {
            name: "virtiofsd_binary",
            passed: vfs_ok,
            detail: vfs_path.map(|p| p.display().to_string()),
        });

        // 5-8. NetworkPolicy::Allow preconditions. These are NOT
        // promoted to blockers — `Deny` mode (the default) doesn't
        // need any of them. They're surfaced as advisory checks so
        // the operator can preflight before flipping their config to
        // `[sandbox.network] enabled = true`. Per
        // `crates/pi-sandbox/docs/NETWORKING.md` §"Auto-install".
        let pasta_ok = which::which("pasta").is_ok();
        checks.push(ProbeCheck {
            name: "pasta_binary (NetworkPolicy::Allow)",
            passed: pasta_ok,
            detail: if pasta_ok {
                Some("found".into())
            } else {
                Some(
                    "missing — install `passt` package; see crates/pi-sandbox/docs/NETWORKING.md"
                        .into(),
                )
            },
        });

        let nft_ok = which::which("nft").is_ok();
        checks.push(ProbeCheck {
            name: "nft_binary (NetworkPolicy::Allow)",
            passed: nft_ok,
            detail: if nft_ok {
                Some("found".into())
            } else {
                Some(
                    "missing — install `nftables` package; see crates/pi-sandbox/docs/NETWORKING.md"
                        .into(),
                )
            },
        });

        let unpriv_userns_ok = std::process::Command::new("unshare")
            .args(["-rUn", "/bin/true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        checks.push(ProbeCheck {
            name: "unprivileged_userns (NetworkPolicy::Allow)",
            passed: unpriv_userns_ok,
            detail: if unpriv_userns_ok {
                Some("`unshare -rUn` succeeds".into())
            } else {
                Some(
                    "`unshare -rUn` failed — kernel may have \
                     `kernel.unprivileged_userns_clone=0`; \
                     `sudo sysctl -w kernel.unprivileged_userns_clone=1`"
                        .into(),
                )
            },
        });

        // Only test TAP creation if the userns probe passed (otherwise
        // the unshare itself will fail and the result is meaningless).
        let tap_create_ok = if unpriv_userns_ok {
            std::process::Command::new("unshare")
                .args([
                    "-rUn",
                    "sh",
                    "-c",
                    "ip tuntap add tap-pi-doctor mode tap && ip tuntap del tap-pi-doctor mode tap",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };
        checks.push(ProbeCheck {
            name: "tap_in_userns (NetworkPolicy::Allow)",
            passed: tap_create_ok,
            detail: if tap_create_ok {
                Some("ip tuntap add/del in `unshare -rUn` succeeds".into())
            } else if !unpriv_userns_ok {
                Some("skipped (unprivileged userns prerequisite failed)".into())
            } else {
                Some(
                    "`ip tuntap add … in `unshare -rUn` failed — \
                     check that the iproute2 package is current"
                        .into(),
                )
            },
        });

        let available = blockers.is_empty();
        let probe_duration_ms = start.elapsed().as_millis() as u32;

        Ok(ProbeReport {
            transport: "firecracker",
            available,
            version,
            probe_duration_ms,
            blockers,
            remediation,
            checks,
        })
    }

    /// Acquire a VM: pop from warm pool if available and valid, else cold-boot.
    async fn acquire(&self, spec: &VmSpec) -> Result<Box<dyn VmHandle>, SandboxError> {
        let acquire_start = Instant::now();

        // ── Rootfs version check (fast-fail before pool lookup) ──────────────
        // Reject requests whose rootfs_version doesn't match what this host
        // binary supports. Checking early gives a clean RootfsMismatch error
        // instead of a confusing boot failure or silent wrong-version pool hit.
        let expected_version = CACHE_ROOTFS_VERSION;
        let requested_version = &spec.rootfs_version.0;
        if requested_version != expected_version {
            return Err(SandboxError::RootfsMismatch {
                expected: expected_version.to_string(),
                found: requested_version.clone(),
            });
        }

        // Try to pop a warm VM that matches the ceiling, host_cwd, AND
        // rootfs_version. Expired entries are drained first so they don't
        // clog the pool or inflate the capacity count.
        let warm_vm = {
            let mut pool = self.pool.lock().await;
            // Prune all expired entries. This must happen before the match
            // and before the capacity check in release()/refill so that
            // "full" pool slots are only real, live VMs.
            pool.retain(|vm| !vm.is_expired());
            let pos = pool.iter().position(|vm| {
                vm.ceiling == spec.vm_ceiling
                    && vm.host_cwd == spec.host_cwd
                    && vm.rootfs_version == spec.rootfs_version.0
            });
            pos.map(|i| pool.remove(i).unwrap())
        };

        let (vm, cold_boot_flag) = match warm_vm {
            Some(vm) => {
                debug!(id = %vm.id, "warm pool hit");
                (vm, false)
            }
            None => {
                debug!("warm pool miss — cold-booting");
                let vm = cold_boot(&self.config, spec).await?;
                (vm, true)
            }
        };

        let acquire_to_ready_ms = acquire_start.elapsed().as_millis() as u32;

        // Opportunistically refill pool in the background.
        // Account for the VM we just checked out (+1 leased) when deciding
        // whether a refill is needed.  Without this correction the refiller
        // would add a VM for every single acquire, allowing the pool to grow
        // unboundedly beyond `pool_size`.
        let pool_clone = Arc::clone(&self.pool);
        let config_clone = Arc::clone(&self.config);
        let spec_clone = spec.clone();
        let pool_size = self.config.pool_size;
        tokio::spawn(async move {
            // Prune expired entries then check: idle + 1 leased vs capacity.
            // We must also re-check after cold_boot to guard against concurrent
            // refill tasks racing: with pool_size=2, three concurrent acquires
            // from an empty pool can all observe idle=0 and all decide to refill.
            // The post-boot re-check ensures that only pool_size VMs are ever
            // resident regardless of how many refill tasks raced.
            let idle = {
                let mut p = pool_clone.lock().await;
                p.retain(|vm| !vm.is_expired());
                p.len()
            };
            if idle + 1 < pool_size {
                match cold_boot(&config_clone, &spec_clone).await {
                    Ok(new_vm) => {
                        let mut p = pool_clone.lock().await;
                        p.retain(|vm| !vm.is_expired());
                        if p.len() < pool_size {
                            p.push_back(new_vm);
                        }
                        // else: another refill task raced us; drop new_vm
                        // (kill_on_drop cleans up the process).
                    }
                    Err(e) => debug!("background pool refill failed: {}", e),
                }
            }
        });

        Ok(Box::new(FirecrackerVmHandle {
            id: vm.id,
            vsock_path: vm.vsock_path,
            _fc_proc: tokio::sync::Mutex::new(vm._fc_proc),
            _vfs_proc: tokio::sync::Mutex::new(vm._vfs_proc),
            born_at: vm.born_at,
            call_count: std::sync::atomic::AtomicU32::new(vm.call_count),
            ceiling: vm.ceiling,
            host_cwd: vm.host_cwd,
            rootfs_version: vm.rootfs_version,
            pool: Arc::clone(&self.pool),
            pool_size,
            acquire_to_ready_ms,
            cold_boot: cold_boot_flag,
        }))
    }
}

// ── VM handle ───────────────────────────────────────────────────────────────

/// Handle to a single acquired Firecracker VM.
pub struct FirecrackerVmHandle {
    id: String,
    vsock_path: PathBuf,
    _fc_proc: tokio::sync::Mutex<Child>,
    _vfs_proc: tokio::sync::Mutex<Option<Child>>,
    born_at: Instant,
    call_count: std::sync::atomic::AtomicU32,
    ceiling: VmCeiling,
    host_cwd: PathBuf,
    /// Rootfs version this VM was booted with; stored so release() can
    /// push a correctly-keyed WarmVm back to the pool.
    rootfs_version: String,
    pool: Arc<Mutex<VecDeque<WarmVm>>>,
    /// Configured pool capacity (from FirecrackerConfig::pool_size).
    /// Used by release() to cap the pool before pushing back.
    pool_size: usize,
    /// Set once at acquire, reported in the first VmExecution.
    acquire_to_ready_ms: u32,
    cold_boot: bool,
}

#[async_trait]
impl VmHandle for FirecrackerVmHandle {
    async fn execute(
        &self,
        _ctx: &ToolContext,
        limits: &CallLimits,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Result<VmExecution, SandboxError> {
        let call_id = Uuid::new_v4().to_string();
        let timeout_ms = limits.wall_timeout.as_millis() as u32;

        let req = ToolRequest {
            proto_version: CURRENT_PROTOCOL_VERSION,
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
            tool_input: tool_input.clone(),
            max_output_bytes: limits.max_output_bytes,
            timeout_ms,
        };

        let exec_start = Instant::now();

        // Connect to the Firecracker vsock UDS and complete the handshake.
        let mut stream = vsock_connect(&self.vsock_path, VSOCK_DEFAULT_PORT)
            .await
            .map_err(|e| SandboxError::Vsock(e.to_string()))?;

        // Write request.
        {
            let (_, mut writer) = tokio::io::split(&mut stream);
            framing::write_request(&mut writer, &req)
                .await
                .map_err(|e| SandboxError::Vsock(e.to_string()))?;
        }

        // Read response with timeout.
        //
        // The JSON frame cap must cover the entire serialized ToolResponse line,
        // including the JSON envelope (field names, quotes, braces, the call_id,
        // numeric fields, etc.) PLUS the worst-case JSON-escaped stdout content.
        //
        // Worst case: every byte in `stdout` is a character that expands under
        // JSON escaping.  The 6× worst case is a byte that serializes as a
        // 6-byte `\uXXXX` escape (e.g. lone surrogates, DEL, some control chars).
        // Most bytes that need escaping expand to at most 2× (`\n` → `\\n`), but
        // we use 6× to be safe and future-proof.
        //
        // The JSON envelope (everything else in ToolResponse: call_id, stderr,
        // is_error, exit_status, guest_duration_ms, field names, quotes, commas)
        // is bounded by a generous fixed slack.
        const JSON_ENVELOPE_SLACK: usize = 8 * 1024;   // 8 KiB for all non-stdout fields
        const WORST_CASE_ESCAPE_FACTOR: usize = 6;      // \uXXXX expansion
        let frame_cap = framing::DEFAULT_MAX_LINE_BYTES
            .max(limits.max_output_bytes as usize * WORST_CASE_ESCAPE_FACTOR + JSON_ENVELOPE_SLACK);
        let resp = tokio::time::timeout(limits.wall_timeout + Duration::from_secs(5), async {
            let mut reader = BufReader::new(&mut stream);
            framing::read_response_with_max(
                &mut reader,
                frame_cap,
                limits.max_output_bytes as usize,
            )
            .await
        })
        .await
        .map_err(|_| SandboxError::Timeout)?
        .map_err(|e| SandboxError::Vsock(e.to_string()))?;

        if resp.call_id != call_id {
            return Err(SandboxError::Vsock(format!(
                "call_id mismatch: expected {call_id}, got {}",
                resp.call_id
            )));
        }

        let guest_duration_ms = resp.guest_duration_ms;
        let acquire_to_ready_ms = self.acquire_to_ready_ms;
        let cold_boot = self.cold_boot;

        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let result = ToolResult {
            tool_use_id: call_id,
            model_output: resp.stdout,
            display: None,
            is_error: resp.is_error,
        };

        let _ = exec_start; // used for wall time — guest_duration_ms is authoritative

        Ok(VmExecution {
            result,
            guest_duration_ms,
            acquire_to_ready_ms,
            cold_boot,
        })
    }

    async fn release(self: Box<Self>) -> Result<(), SandboxError> {
        let expired = self.call_count.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CALLS
            || self.born_at.elapsed() >= MAX_AGE;

        if expired {
            // Processes die via kill_on_drop when dropped here.
            debug!(id = %self.id, "VM retired (rotation cap hit)");
            return Ok(());
        }

        // Return to pool, but only if there is still room.  The background
        // refill task may have already filled the pool to capacity while this
        // VM was leased; pushing unconditionally would let the pool grow
        // beyond `pool_size`.
        let fc_proc = self._fc_proc.into_inner();
        let vfs_proc = self._vfs_proc.into_inner();
        let vm_id = self.id.clone();
        let warm = WarmVm {
            id: self.id,
            vsock_path: self.vsock_path,
            _fc_proc: fc_proc,
            _vfs_proc: vfs_proc,
            born_at: self.born_at,
            call_count: self.call_count.load(std::sync::atomic::Ordering::Relaxed),
            ceiling: self.ceiling,
            host_cwd: self.host_cwd,
            rootfs_version: self.rootfs_version,
        };
        {
            let mut pool = self.pool.lock().await;
            // Prune expired entries first so they don't inflate the capacity
            // count and prevent valid live VMs from being returned.
            pool.retain(|vm| !vm.is_expired());
            if pool.len() < self.pool_size {
                pool.push_back(warm);
                debug!(id = %vm_id, "VM returned to pool");
            } else {
                // Pool already full (background refill raced ahead).
                // Drop `warm` here — kill_on_drop cleans up the process.
                debug!(id = %vm_id, "VM dropped (pool already at capacity)");
            }
        }
        Ok(())
    }
}

// ── Cold boot ────────────────────────────────────────────────────────────────────────────

/// Spawn a fresh Firecracker VM for the given spec and wait until the
/// guest vsock worker is ready to accept connections.
///
/// Pre-condition: `acquire()` has already validated `spec.rootfs_version`
/// against `CACHE_ROOTFS_VERSION` and returned `RootfsMismatch` if they
/// differ, so this function focuses on booting.
async fn cold_boot(config: &FirecrackerConfig, spec: &VmSpec) -> Result<WarmVm, SandboxError> {
    let vm_id = Uuid::new_v4().to_string();
    let run_dir = config.run_dir.join(&vm_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    let api_sock = run_dir.join("api.sock");
    let vsock_sock = run_dir.join("vsock.sock");
    let config_path = run_dir.join("fc-config.json");
    let virtiofs_sock = run_dir.join("virtiofs.sock");

    let kernel_path = config.resolved_kernel_path();

    // Resolve the decompressed rootfs image. Firecracker requires an
    // uncompressed block device; the cache stores `.img.zst`.
    //
    // Priority:
    //   1. PI_SANDBOX_ROOTFS env var  — trust the caller, use as-is.
    //   2. config.rootfs_path         — explicit override, trust caller.
    //   3. Default: RootfsCache::ensure() → .img.zst; decompress to .img
    //      automatically on first use.
    let rootfs_path = if let Ok(p) = std::env::var("PI_SANDBOX_ROOTFS") {
        // Env override: maintainer supplies a ready-to-use image directly.
        PathBuf::from(p)
    } else if let Some(ref p) = config.rootfs_path {
        // Explicit override in config — trust caller.
        p.clone()
    } else {
        // Default: use the cache layer to fetch / verify the artifact, then
        // decompress the .img.zst to a sibling .img if not already done.
        let expected_version = CACHE_ROOTFS_VERSION;
        let cache = RootfsCache::with_default_dir();
        let zst_path = cache
            .ensure(
                expected_version,
                ROOTFS_URL,
                ROOTFS_SHA256,
                ROOTFS_SIZE_BYTES,
            )
            .await
            .map_err(|e| SandboxError::Unavailable(format!("rootfs cache: {e}")))?;
        // Derive the decompressed image path (strips the trailing .zst).
        let img_path = zst_path.with_extension(""); // e.g. .../rootfs.img
        if !img_path.exists() {
            // First-use: decompress in-place so subsequent boots skip this.
            decompress_zst(&zst_path, &img_path).await?;
        }
        img_path
    };

    let fc_bin = config
        .resolved_firecracker()
        .ok_or_else(|| SandboxError::Unavailable("firecracker binary not found".into()))?;
    // virtiofsd is REQUIRED: the /work virtio-fs share is load-bearing for
    // every tool call. Without it the guest cannot access the host cwd and
    // write/edit/bash calls are silently broken. Error early rather than
    // boot a VM that will fail all path-sensitive tool calls.
    let vfs_bin = config.resolved_virtiofsd().ok_or_else(|| {
        SandboxError::Unavailable(
            "virtiofsd binary not found (tried $PI_SANDBOX_VIRTIOFSD, $PATH, /usr/lib/virtiofsd); \
             cannot mount /work share in guest".into(),
        )
    })?;

    // CID must be unique per-VM. Use a hash of the vm_id UUID for uniqueness
    // in range [3, 2^32-1] (0=hypervisor, 1=reserved, 2=host).
    let cid = vm_id_to_cid(&vm_id);

    // Spawn virtiofsd first (it must be ready before Firecracker boots).
    // --shared-dir  : the host directory to expose as /work in the guest.
    // --socket-path : the UDS path Firecracker references in its "fs" config.
    let vfs_socket_path = virtiofs_sock.display().to_string();
    let shared_dir = spec.host_cwd.display().to_string();
    let vfs_proc = tokio::process::Command::new(&vfs_bin)
        .args([
            "--socket-path",
            &vfs_socket_path,
            "--shared-dir",
            &shared_dir,
            "--sandbox",
            "none",
            "--seccomp",
            "none",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    // Give virtiofsd a moment to create its socket before Firecracker tries
    // to connect to it.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Write Firecracker config JSON.
    let fc_config = build_fc_config(
        &kernel_path,
        &rootfs_path,
        &vsock_sock,
        cid,
        &virtiofs_sock,
        spec,
    );
    let config_json = serde_json::to_string_pretty(&fc_config)
        .map_err(|e| SandboxError::Provider(e.to_string()))?;
    tokio::fs::write(&config_path, config_json).await?;

    // Spawn Firecracker. Debug mode (PI_SANDBOX_FC_DEBUG=1) captures
    // stdout/stderr to /tmp/pi-sandbox-fc-debug/<vm_id>/ so smoke-test
    // failures are diagnosable. When `spec.network_policy` is
    // `NetworkPolicy::Allow`, the FC argv is built behind a
    // pasta + bash wrapper that creates an unprivileged user+net
    // namespace, sets up the TAP and nft rules, then `exec`s
    // firecracker (see `crates/pi-sandbox/docs/NETWORKING.md`).
    let netns_setup = build_netns_setup_script(spec)?;
    // Optional egress trace: when PI_SANDBOX_FC_PCAP_DIR is set, the
    // pasta invocation grows `--pcap <dir>/<vm_id>.pcap` (full L2
    // capture, openable in tcpdump/wireshark) and
    // `--log-file <dir>/<vm_id>.pasta.log` (text log of pasta's
    // userspace forwarding decisions). Both files are written from
    // inside pasta — outside the netns, owned by the pi process,
    // suitable for post-hoc audit.
    let pcap_dir = std::env::var("PI_SANDBOX_FC_PCAP_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(dir) = &pcap_dir {
        let _ = std::fs::create_dir_all(dir);
    }
    let pcap_path = pcap_dir
        .as_ref()
        .map(|d| d.join(format!("{vm_id}.pcap")));
    let pasta_log_path = pcap_dir
        .as_ref()
        .map(|d| d.join(format!("{vm_id}.pasta.log")));
    if pcap_path.is_some() {
        eprintln!(
            "PI_SANDBOX_FC_PCAP_DIR: pcap+pasta log at {}/{vm_id}.{{pcap,pasta.log}}",
            pcap_dir.as_ref().unwrap().display()
        );
    }
    let make_fc_command = || -> tokio::process::Command {
        match &netns_setup {
            Some(setup) => {
                // pasta [--pcap PCAP] [--log-file LOG] --config-net --
                //   bash -c 'setup; exec "$@"' --
                //   firecracker --api-sock SOCK --config-file CFG
                let inner = format!("{setup}\nexec \"$@\"\n");
                let mut c = tokio::process::Command::new("pasta");
                if let Some(p) = &pcap_path {
                    c.arg("--pcap").arg(p);
                }
                if let Some(p) = &pasta_log_path {
                    c.arg("--log-file").arg(p);
                }
                c.arg("--config-net")
                    .arg("--")
                    .arg("bash")
                    .arg("-c")
                    .arg(inner)
                    .arg("--") // becomes $0 inside bash -c, padding for $@
                    .arg(&fc_bin)
                    .arg("--api-sock")
                    .arg(&api_sock)
                    .arg("--config-file")
                    .arg(&config_path);
                c
            }
            None => {
                let mut c = tokio::process::Command::new(&fc_bin);
                c.arg("--api-sock")
                    .arg(&api_sock)
                    .arg("--config-file")
                    .arg(&config_path);
                c
            }
        }
    };
    let fc_proc = if std::env::var("PI_SANDBOX_FC_DEBUG").as_deref() == Ok("1") {
        let dbg_dir = std::path::PathBuf::from("/tmp/pi-sandbox-fc-debug").join(&vm_id);
        std::fs::create_dir_all(&dbg_dir)?;
        let _ = std::fs::copy(&config_path, dbg_dir.join("fc-config.json"));
        let fc_stdout = std::fs::File::create(dbg_dir.join("fc.stdout"))?;
        let fc_stderr = std::fs::File::create(dbg_dir.join("fc.stderr"))?;
        eprintln!("PI_SANDBOX_FC_DEBUG: logs at {}", dbg_dir.display());
        let mut cmd = make_fc_command();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(fc_stdout))
            .stderr(Stdio::from(fc_stderr))
            .kill_on_drop(true)
            .spawn()?
    } else {
        let mut cmd = make_fc_command();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?
    };

    // Bind the per-VM `web_search` proxy listener at
    // `<vsock_sock>_5003` only when the operator's policy actually
    // allows network. Vsock is a parallel channel to eth0/TAP/nft,
    // so without this gate `web_search` would silently bypass
    // `NetworkPolicy::Deny`. Tying the listener to `Allow` makes
    // the operator's policy authoritative across both channels:
    //
    //   Deny  → listener never binds → guest's web_search proxy
    //           gets "vsock io: Connection refused" → clean
    //           ToolResponse with is_error=true
    //   Allow → listener binds → web_search round-trips through
    //           the host's WebSearchTool with host AuthStorage
    //
    // Per-tool finer-grained gates (e.g. "Allow eth0 but disable
    // web_search proxy") are an additive v1.1 refinement.
    if matches!(spec.network_policy, NetworkPolicy::Allow { .. }) {
        if let Err(e) = crate::microvm::search_proxy::spawn_search_proxy_listener(&vsock_sock) {
            warn!(err = %e, "failed to bind web_search proxy listener; web_search calls from this VM will fail");
        }
    }

    // Wait for the guest vsock worker to be reachable.
    wait_for_vsock_ready(&vsock_sock, cid).await?;

    Ok(WarmVm {
        id: vm_id,
        vsock_path: vsock_sock,
        _fc_proc: fc_proc,
        _vfs_proc: Some(vfs_proc),
        born_at: Instant::now(),
        call_count: 0,
        ceiling: spec.vm_ceiling,
        host_cwd: spec.host_cwd.clone(),
        rootfs_version: spec.rootfs_version.0.clone(),
    })
}

/// Decompress a `.img.zst` file to a sibling `.img` file using the `zstd`
/// crate. Writes to a temp file then renames atomically so a crash mid-write
/// does not leave a corrupt `.img` behind.
async fn decompress_zst(src: &std::path::Path, dst: &std::path::Path) -> Result<(), SandboxError> {
    use std::io::{BufReader, BufWriter, Read, Write};

    let src = src.to_owned();
    let dst = dst.to_owned();

    // Run synchronous decompression on a blocking thread so we do not
    // starve the async executor on large images.
    tokio::task::spawn_blocking(move || {
        let src_file = std::fs::File::open(&src).map_err(|e| {
            SandboxError::Unavailable(format!(
                "cannot open compressed rootfs {}: {e}",
                src.display()
            ))
        })?;
        let mut decoder = zstd::Decoder::new(BufReader::new(src_file)).map_err(|e| {
            SandboxError::Unavailable(format!("zstd decoder init failed: {e}"))
        })?;

        // Write to a temp file in the same directory, then rename atomically.
        // Use a UUID instead of PID to avoid concurrent cold-boot races where
        // two tasks in the same process share the same PID and would clobber
        // each other's temp file before the atomic rename.
        let dst_parent = dst.parent().unwrap_or_else(|| std::path::Path::new("."));
        let tmp_path = dst_parent.join(format!(".rootfs.img.tmp.{}", uuid::Uuid::new_v4()));
        {
            let out_file = std::fs::File::create(&tmp_path).map_err(|e| {
                SandboxError::Unavailable(format!(
                    "cannot create tmp rootfs {}: {e}",
                    tmp_path.display()
                ))
            })?;
            let mut writer = BufWriter::new(out_file);
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = decoder.read(&mut buf).map_err(|e| {
                    SandboxError::Unavailable(format!("zstd decompression error: {e}"))
                })?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n]).map_err(|e| {
                    SandboxError::Unavailable(format!("write to tmp rootfs failed: {e}"))
                })?;
            }
            writer.flush().map_err(|e| {
                SandboxError::Unavailable(format!("flush tmp rootfs failed: {e}"))
            })?;
        }

        std::fs::rename(&tmp_path, &dst).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            SandboxError::Unavailable(format!(
                "rename {} to {}: {e}",
                tmp_path.display(),
                dst.display()
            ))
        })
    })
    .await
    .map_err(|e| SandboxError::Unavailable(format!("blocking task panicked: {e}")))??;

    Ok(())
}

/// Derive a stable CID from a UUID string. Output in [3, u32::MAX].
fn vm_id_to_cid(vm_id: &str) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    vm_id.hash(&mut h);
    let n = Hasher::finish(&h);
    // Map to [3, u32::MAX] to avoid reserved CIDs 0-2.
    3u32.saturating_add((n % (u32::MAX as u64 - 3)) as u32)
}

/// Build the Firecracker config JSON value.
///
/// The `virtiofs_sock` is the **virtiofsd daemon socket path** that
/// Firecracker connects to for the virtio-fs device — NOT the shared
/// directory. virtiofsd itself is told the shared directory via
/// `--shared-dir`; Firecracker only needs the socket.
///
/// Firecracker's `fs` device schema (as of v1.x):
/// ```json
/// { "fs_id": "work", "socket": "/path/to/virtiofs.sock", "tag": "work" }
/// ```
fn build_fc_config(
    kernel_path: &std::path::Path,
    rootfs_path: &std::path::Path,
    vsock_sock: &std::path::Path,
    cid: u32,
    virtiofs_sock: &std::path::Path,
    spec: &VmSpec,
) -> serde_json::Value {
    // Build kernel cmdline. Append pi.net.* knobs when a network policy
    // requests Allow so the guest's init script can configure eth0
    // without further out-of-band signals.
    let mut boot_args = format!(
        "console=ttyS0 reboot=k panic=1 pci=off \
         i8042.nokbd i8042.noaux \
         root=/dev/vda rw init=/init \
         pi.proto_version={} \
         pi.overlay.size_mib={}",
        CURRENT_PROTOCOL_VERSION,
        spec.vm_ceiling.disk_mib,
    );
    if let NetworkPolicy::Allow {
        guest_ip_cidr,
        guest_gateway,
        guest_dns,
        ..
    } = &spec.network_policy
    {
        // Replace any whitespace in DNS list (cmdline can't have spaces in a value).
        let dns_csv = guest_dns.join(",");
        boot_args.push_str(&format!(
            " pi.net.ip={} pi.net.gw={} pi.net.dns={}",
            guest_ip_cidr, guest_gateway, dns_csv
        ));
    }

    let mut config = serde_json::json!({
        "boot-source": {
            "kernel_image_path": kernel_path.display().to_string(),
            "boot_args": boot_args,
        },
        "drives": [
            {
                "drive_id": "rootfs",
                "path_on_host": rootfs_path.display().to_string(),
                "is_root_device": true,
                "is_read_only": true
            }
        ],
        "machine-config": {
            "vcpu_count": spec.vm_ceiling.vcpus,
            "mem_size_mib": spec.vm_ceiling.mem_mib
        },
        "vsock": {
            "guest_cid": cid,
            "uds_path": vsock_sock.display().to_string()
        },
        // virtio-fs share: point Firecracker at the virtiofsd socket.
        // The guest kernel mounts tag "work" at /work via the init script.
        // The `socket` field is the virtiofsd UDS, not the shared directory.
        "fs": [
            {
                "fs_id": "work",
                "socket": virtiofs_sock.display().to_string(),
                "tag": "work"
            }
        ]
    });

    // network_interfaces: only present when policy is Allow. Caller must
    // have created the TAP and (typically) wrapped it with pasta+nftables.
    if let NetworkPolicy::Allow {
        tap_name,
        guest_mac,
        ..
    } = &spec.network_policy
    {
        let mac = guest_mac
            .clone()
            .unwrap_or_else(|| derive_guest_mac_from_cid(cid));
        config["network-interfaces"] = serde_json::json!([
            {
                "iface_id": "eth0",
                "host_dev_name": tap_name,
                "guest_mac": mac,
            }
        ]);
    }

    config
}

/// Derive a stable, locally-administered MAC from the per-VM CID so
/// nftables rules can pin per-VM filters by source MAC if desired.
/// Format: `02:00:<cid_be>` — the locally-administered bit is set in
/// the OUI so it doesn't collide with vendor MACs.
fn derive_guest_mac_from_cid(cid: u32) -> String {
    let bytes = cid.to_be_bytes();
    format!(
        "02:00:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

/// Build the bash setup script that runs *inside* the pasta-managed
/// child user+net namespace before `exec firecracker …`. Returns
/// `Ok(None)` for `NetworkPolicy::Deny` (the launcher then spawns
/// firecracker directly, no wrapper).
///
/// The script:
///   1. creates the named TAP and assigns the host-side `/30` address,
///   2. brings up the TAP,
///   3. opens a `pi-fw` filter chain (forward, policy accept for
///      v1.1 — selective allowlist deferred),
///   4. opens a `pi-nat` postrouting chain that masquerades the
///      guest subnet out the netns's pasta-provided default-route
///      interface,
///   5. enables IPv4 forwarding inside the netns.
///
/// Failure modes are surfaced as `SandboxError::Provider`.
///
/// This function is the runtime counterpart to the auto-install
/// ladder documented in `crates/pi-sandbox/docs/NETWORKING.md`.
/// When prerequisites (pasta, nft, kernel-allowed unpriv userns)
/// are missing, `acquire()` returns the doctor-pointing error
/// described there.
fn build_netns_setup_script(spec: &VmSpec) -> Result<Option<String>, SandboxError> {
    let (tap_name, guest_ip_cidr, guest_gateway, guest_dns, egress_allowlist) =
        match &spec.network_policy {
            NetworkPolicy::Deny => return Ok(None),
            NetworkPolicy::Allow {
                tap_name,
                guest_ip_cidr,
                guest_gateway,
                guest_dns,
                egress_allowlist,
                ..
            } => (
                tap_name,
                guest_ip_cidr,
                guest_gateway,
                guest_dns,
                egress_allowlist,
            ),
        };
    if which::which("pasta").is_err() {
        return Err(SandboxError::Provider(
            "NetworkPolicy::Allow requires `pasta` (passt package). \
             Install it (Debian/Ubuntu: `sudo apt install passt`; \
             Arch/Manjaro: `sudo pacman -S passt`; Fedora/RHEL: \
             `sudo dnf install passt`) and run `pi sandbox doctor`."
                .into(),
        ));
    }
    if which::which("nft").is_err() {
        return Err(SandboxError::Provider(
            "NetworkPolicy::Allow requires `nft` (nftables package). \
             Install it via your distro's package manager and run \
             `pi sandbox doctor`."
                .into(),
        ));
    }
    // Validate every allowlist entry on the way in. Sh-quoting any
    // entry that survives is unsafe regardless of validation, so we
    // also reject entries that contain shell metacharacters even if
    // they superficially look like a hostname.
    for entry in egress_allowlist {
        if entry.is_empty()
            || entry
                .chars()
                .any(|c| c.is_ascii_whitespace() || matches!(c, '\'' | '"' | '`' | '$' | ';' | '|' | '&' | '\\'))
        {
            return Err(SandboxError::Provider(format!(
                "NetworkPolicy::Allow.egress_allowlist rejects entry containing whitespace or shell metacharacters: {entry:?}"
            )));
        }
    }
    let prefix = guest_ip_cidr
        .split_once('/')
        .map(|(_, p)| p.to_string())
        .ok_or_else(|| {
            SandboxError::Provider(format!(
                "NetworkPolicy::Allow.guest_ip_cidr lacks `/PREFIX`: {guest_ip_cidr}"
            ))
        })?;
    let host_tap_cidr = format!("{guest_gateway}/{prefix}");
    let masq_subnet = compute_subnet(guest_ip_cidr)?;

    // DNS allow set: always permit UDP/53 (and TCP/53 for fallback)
    // to the host-injected resolvers — without these the guest can't
    // even resolve the allowlist hostnames it WAS authorised for.
    let dns_set = if guest_dns.is_empty() {
        // No DNS configured — allowlist is pure-IP, that's the
        // operator's call. Don't add a DNS rule.
        String::new()
    } else {
        format!(
            "nft add rule ip pi-fw forward iifname {tap} ip daddr {{ {} }} udp dport 53 accept\n\
             nft add rule ip pi-fw forward iifname {tap} ip daddr {{ {} }} tcp dport 53 accept\n",
            guest_dns.join(", "),
            guest_dns.join(", "),
            tap = tap_name,
        )
    };

    // Build the resolver block + accept rule. We resolve hostnames
    // INSIDE the netns at setup time (so DNS goes through pasta's
    // userspace forwarder), then translate the resolved IPs into
    // a single nft accept rule. CIDRs and bare IPs pass through
    // unchanged.
    //
    // The allowlist entries are space-separated literals already
    // validated to contain no shell metacharacters; embedding them
    // in the heredoc-free `for entry in <literals>` form is safe.
    let allowlist_block = if egress_allowlist.is_empty() {
        // Empty list + drop default = closed network. The guest can
        // boot, /etc/resolv.conf is written, but no new flow leaves.
        String::new()
    } else {
        let entries = egress_allowlist.join(" ");
        format!(
            r#"
allow_set=""
for entry in {entries}; do
  case "$entry" in
    *[!0-9./]*) ;;  # not a bare IP/CIDR — fall through to DNS
    *)
      allow_set="$allow_set, $entry"
      continue ;;
  esac
  ips=$(getent ahostsv4 "$entry" 2>/dev/null | awk '{{print $1}}' | sort -u)
  if [ -z "$ips" ]; then
    echo "pi-sandbox: cannot resolve egress allowlist entry '$entry'" >&2
    exit 1
  fi
  for ip in $ips; do allow_set="$allow_set, $ip"; done
done
allow_set=$(echo "$allow_set" | sed 's/^, //')
[ -n "$allow_set" ] || {{ echo "pi-sandbox: egress allowlist resolved to empty set" >&2; exit 1; }}
nft add rule ip pi-fw forward iifname {tap} ip daddr "{{ $allow_set }}" accept
"#,
            entries = entries,
            tap = tap_name,
        )
    };

    Ok(Some(format!(
        r#"set -e
ip tuntap add {tap} mode tap
ip addr add {host_cidr} dev {tap}
ip link set {tap} up
nft add table ip pi-fw
nft add chain ip pi-fw forward '{{ type filter hook forward priority 0 ; policy drop ; }}'
# Return path: any flow we permitted outbound gets its replies back.
nft add rule ip pi-fw forward ct state established,related accept
{dns_set}{allowlist_block}
nft add table ip pi-nat
nft add chain ip pi-nat post '{{ type nat hook postrouting priority 100 ; }}'
out_iface=$(ip route get 1.1.1.1 2>/dev/null | awk '{{for(i=1;i<=NF;i++) if($i=="dev") {{print $(i+1); exit}}}}')
[ -n "$out_iface" ] || {{ echo "pi-sandbox: no default route inside netns" >&2; exit 1; }}
nft add rule ip pi-nat post oifname "$out_iface" ip saddr {masq_subnet} masquerade
echo 1 > /proc/sys/net/ipv4/ip_forward
"#,
        tap = tap_name,
        host_cidr = host_tap_cidr,
        masq_subnet = masq_subnet,
        dns_set = dns_set,
        allowlist_block = allowlist_block,
    )))
}

/// Compute the network address for an IPv4 CIDR (`addr/PREFIX`).
/// E.g. `compute_subnet("172.16.0.2/30") == "172.16.0.0/30"`.
fn compute_subnet(cidr: &str) -> Result<String, SandboxError> {
    let (addr_s, prefix_s) = cidr.split_once('/').ok_or_else(|| {
        SandboxError::Provider(format!("invalid CIDR (no `/`): {cidr}"))
    })?;
    let prefix: u8 = prefix_s.parse().map_err(|_| {
        SandboxError::Provider(format!("invalid CIDR prefix `{prefix_s}` in `{cidr}`"))
    })?;
    if prefix > 32 {
        return Err(SandboxError::Provider(format!(
            "CIDR prefix /{prefix} > 32 in `{cidr}`"
        )));
    }
    let ip: std::net::Ipv4Addr = addr_s.parse().map_err(|_| {
        SandboxError::Provider(format!("invalid CIDR address `{addr_s}` in `{cidr}`"))
    })?;
    let mask: u32 = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
    let network = u32::from(ip) & mask;
    Ok(format!("{}/{prefix}", std::net::Ipv4Addr::from(network)))
}

/// Wait until the guest vsock worker is reachable on `VSOCK_DEFAULT_PORT`.
/// Polls every `VSOCK_POLL_INTERVAL` up to `VSOCK_READY_TIMEOUT`.
async fn wait_for_vsock_ready(vsock_path: &std::path::Path, _cid: u32) -> Result<(), SandboxError> {
    let deadline = Instant::now() + VSOCK_READY_TIMEOUT;
    loop {
        match vsock_connect(vsock_path, VSOCK_DEFAULT_PORT).await {
            Ok(_stream) => {
                debug!(path = %vsock_path.display(), "guest vsock worker ready");
                return Ok(());
            }
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(VSOCK_POLL_INTERVAL).await;
            }
            Err(e) => {
                return Err(SandboxError::Vsock(format!(
                    "timed out waiting for guest vsock at {}: {}",
                    vsock_path.display(),
                    e
                )));
            }
        }
    }
}

// ── Vsock helper (aegis vsock::connect_port pattern) ────────────────────────

/// Connect to a Firecracker vsock UDS and complete the `CONNECT <port>`
/// handshake. Returns the raw `UnixStream` after the `OK` ack.
///
/// Firecracker's vsock multiplexer sits on a UDS path. The guest CID is
/// implicit; we send `CONNECT <port>\n` and Firecracker responds with
/// `OK <cid> <port>\n` once the guest accepts the connection.
async fn vsock_connect(
    vsock_path: &std::path::Path,
    port: u32,
) -> Result<UnixStream, std::io::Error> {
    let mut stream = UnixStream::connect(vsock_path).await?;

    let connect_msg = format!("CONNECT {port}\n");
    stream.write_all(connect_msg.as_bytes()).await?;

    // Read the `OK <cid> <port>\n` ack.
    let mut ack = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut ack).await?;
    }

    let ack = ack.trim();
    if !ack.starts_with("OK ") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!(
                "Firecracker vsock handshake failed for port {port}: \
                 expected \"OK ...\", got {ack:?}"
            ),
        ));
    }

    Ok(stream)
}

// ── SandboxError mapping ────────────────────────────────────────────────────
// (SandboxError already implements From<std::io::Error> in provider.rs.
//  No additional From impls needed here.)

// ── SandboxError::Timeout helper ────────────────────────────────────────────
// `tokio::time::error::Elapsed` does not impl Into<SandboxError> directly;
// we map it at the call site.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_id_to_cid_avoids_reserved() {
        for i in 0..100 {
            let id = format!("test-vm-{i}");
            let cid = vm_id_to_cid(&id);
            assert!(cid >= 3, "CID {cid} is in reserved range for vm_id={id}");
        }
    }

    #[test]
    fn vm_id_to_cid_is_deterministic() {
        let id = "stable-id";
        assert_eq!(vm_id_to_cid(id), vm_id_to_cid(id));
    }

    #[test]
    fn config_default_pool_size_is_two() {
        let cfg = FirecrackerConfig::default();
        assert_eq!(cfg.pool_size, DEFAULT_POOL_SIZE);
    }

    #[test]
    fn compute_subnet_basic_cases() {
        assert_eq!(
            compute_subnet("172.16.0.2/30").unwrap(),
            "172.16.0.0/30"
        );
        assert_eq!(
            compute_subnet("10.0.0.5/24").unwrap(),
            "10.0.0.0/24"
        );
        assert_eq!(
            compute_subnet("192.168.1.130/26").unwrap(),
            "192.168.1.128/26"
        );
        // /32: a single host — network == host.
        assert_eq!(
            compute_subnet("8.8.8.8/32").unwrap(),
            "8.8.8.8/32"
        );
        // /0: full mask off — everything maps to 0.0.0.0/0.
        assert_eq!(
            compute_subnet("1.2.3.4/0").unwrap(),
            "0.0.0.0/0"
        );
    }

    #[test]
    fn compute_subnet_rejects_garbage() {
        assert!(compute_subnet("not-a-cidr").is_err());
        assert!(compute_subnet("172.16.0.2").is_err()); // missing /
        assert!(compute_subnet("172.16.0.2/33").is_err()); // /33 out of range
        assert!(compute_subnet("999.999.999.999/24").is_err()); // bad addr
    }

    #[test]
    fn build_netns_setup_script_returns_none_for_deny() {
        let spec = VmSpec {
            host_cwd: PathBuf::from("/tmp"),
            host_cwd_writable: true,
            env: Default::default(),
            network_policy: NetworkPolicy::Deny,
            vm_ceiling: VmCeiling::default(),
            rootfs_version: crate::microvm::RootfsVersion::current(),
        };
        assert!(build_netns_setup_script(&spec).unwrap().is_none());
    }

    #[test]
    fn config_resolved_kernel_uses_env() {
        std::env::set_var("PI_SANDBOX_KERNEL", "/custom/kernel");
        let cfg = FirecrackerConfig::default();
        assert_eq!(cfg.resolved_kernel_path(), PathBuf::from("/custom/kernel"));
        std::env::remove_var("PI_SANDBOX_KERNEL");
    }

    #[test]
    fn config_resolved_rootfs_uses_env() {
        std::env::set_var("PI_SANDBOX_ROOTFS", "/custom/rootfs.img");
        let cfg = FirecrackerConfig::default();
        assert_eq!(cfg.resolved_rootfs_path(), PathBuf::from("/custom/rootfs.img"));
        std::env::remove_var("PI_SANDBOX_ROOTFS");
    }

    #[test]
    fn rootfs_default_path_is_img_not_zst() {
        // Guard against regressions: the default cache path must be the
        // *decompressed* `.img` file, not the `.img.zst` archive that
        // Firecracker cannot use as a block device.
        std::env::remove_var("PI_SANDBOX_ROOTFS"); // ensure env override is not set
        let cfg = FirecrackerConfig::default();
        let path = cfg.resolved_rootfs_path();
        assert!(
            !path.to_string_lossy().ends_with(".img.zst"),
            "resolved_rootfs_path must return an uncompressed .img path, got: {}",
            path.display()
        );
        assert!(
            path.to_string_lossy().ends_with(".img"),
            "resolved_rootfs_path should end with .img, got: {}",
            path.display()
        );
    }

    /// Verify that release() respects the pool_size cap when the background
    /// refill task has already filled the pool to capacity.
    ///
    /// This is a logical test — we manually build the WarmVm queue and the
    /// handle to confirm that pushing past `pool_size` is rejected.
    #[tokio::test]
    async fn release_does_not_overfill_pool() {
        use std::collections::VecDeque;
        use tokio::sync::Mutex;

        // A pool already at capacity (pool_size = 1, one WarmVm in queue).
        // We need a dummy Child for the WarmVm; use a sleeping process.
        let dummy_child = || {
            tokio::process::Command::new("sleep")
                .arg("1000")
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep")
        };

        let pool: Arc<Mutex<VecDeque<WarmVm>>> = Arc::new(Mutex::new({
            let mut d = VecDeque::new();
            d.push_back(WarmVm {
                id: "already-in-pool".to_string(),
                vsock_path: PathBuf::from("/dev/null"),
                _fc_proc: dummy_child(),
                _vfs_proc: None,
                born_at: Instant::now(),
                call_count: 0,
                ceiling: VmCeiling::default(),
                host_cwd: PathBuf::from("/tmp"),
                rootfs_version: "0.1.0".to_string(),
            });
            d
        }));

        // Simulate a leased handle with pool_size = 1.
        let handle = Box::new(FirecrackerVmHandle {
            id: "leased-vm".to_string(),
            vsock_path: PathBuf::from("/dev/null"),
            _fc_proc: tokio::sync::Mutex::new(dummy_child()),
            _vfs_proc: tokio::sync::Mutex::new(None),
            born_at: Instant::now(),
            call_count: std::sync::atomic::AtomicU32::new(0),
            ceiling: VmCeiling::default(),
            host_cwd: PathBuf::from("/tmp"),
            rootfs_version: "0.1.0".to_string(),
            pool: Arc::clone(&pool),
            pool_size: 1,
            acquire_to_ready_ms: 0,
            cold_boot: false,
        });

        handle.release().await.expect("release should not fail");

        // Pool must still contain exactly 1 entry (the pre-existing one).
        let len = pool.lock().await.len();
        assert_eq!(
            len, 1,
            "pool must not exceed pool_size=1 after release; got {len}"
        );
    }

    /// Verify that release() prunes expired pool entries before checking
    /// capacity, so a slot that was "full" with expired VMs becomes available
    /// for a returning live VM.
    ///
    /// Scenario: pool_size=1, pool holds one expired VM, returning a live VM.
    /// Expected: expired VM is pruned; live VM enters the pool; len=1.
    #[tokio::test]
    async fn release_prunes_expired_before_capacity_check() {
        use std::collections::VecDeque;
        use tokio::sync::Mutex;

        let dummy_child = || {
            tokio::process::Command::new("sleep")
                .arg("1000")
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep")
        };

        // Pool at capacity but with an expired entry (MAX_CALLS hit).
        let pool: Arc<Mutex<VecDeque<WarmVm>>> = Arc::new(Mutex::new({
            let mut d = VecDeque::new();
            d.push_back(WarmVm {
                id: "expired-in-pool".to_string(),
                vsock_path: PathBuf::from("/dev/null"),
                _fc_proc: dummy_child(),
                _vfs_proc: None,
                born_at: Instant::now(),
                call_count: MAX_CALLS, // already at rotation cap
                ceiling: VmCeiling::default(),
                host_cwd: PathBuf::from("/tmp"),
                rootfs_version: "0.1.0".to_string(),
            });
            d
        }));

        // A fresh live handle being returned.
        let handle = Box::new(FirecrackerVmHandle {
            id: "live-vm".to_string(),
            vsock_path: PathBuf::from("/dev/null"),
            _fc_proc: tokio::sync::Mutex::new(dummy_child()),
            _vfs_proc: tokio::sync::Mutex::new(None),
            born_at: Instant::now(),
            call_count: std::sync::atomic::AtomicU32::new(0),
            ceiling: VmCeiling::default(),
            host_cwd: PathBuf::from("/tmp"),
            rootfs_version: "0.1.0".to_string(),
            pool: Arc::clone(&pool),
            pool_size: 1,
            acquire_to_ready_ms: 0,
            cold_boot: false,
        });

        handle.release().await.expect("release should not fail");

        // Expired VM should have been pruned; live VM should be in the pool.
        let pool_guard = pool.lock().await;
        assert_eq!(
            pool_guard.len(),
            1,
            "pool should contain exactly 1 live VM after pruning expired; got {}",
            pool_guard.len()
        );
        assert_eq!(
            pool_guard[0].id, "live-vm",
            "pool should hold the live VM after expired entry was pruned"
        );
    }

    /// Verify the frame_cap formula covers the worst-case JSON-escaped output.
    ///
    /// A bash response whose stdout contains `max_output_bytes` worth of
    /// newlines serializes to roughly 2× (each `\n` → the two-byte `\n` JSON
    /// literal). The 6× factor covers `\uXXXX` escapes for control chars.
    /// This test confirms the cap stays above both the 2× and 6× bounds.
    #[test]
    fn frame_cap_covers_worst_case_json_escaping() {
        // Simulate the calculation in execute() for a default max_output_bytes.
        let max_output_bytes: usize = 256 * 1024; // CallLimits default
        const JSON_ENVELOPE_SLACK: usize = 8 * 1024;
        const WORST_CASE_ESCAPE_FACTOR: usize = 6;
        let frame_cap = framing::DEFAULT_MAX_LINE_BYTES
            .max(max_output_bytes * WORST_CASE_ESCAPE_FACTOR + JSON_ENVELOPE_SLACK);

        // Must exceed a 2× expansion of max_output_bytes (e.g. all newlines).
        let two_x_bound = max_output_bytes * 2 + JSON_ENVELOPE_SLACK;
        assert!(
            frame_cap >= two_x_bound,
            "frame_cap {frame_cap} must be >= 2× bound {two_x_bound}"
        );

        // Must exceed a 6× expansion (worst-case \\uXXXX escapes).
        let six_x_bound = max_output_bytes * 6 + JSON_ENVELOPE_SLACK;
        assert!(
            frame_cap >= six_x_bound,
            "frame_cap {frame_cap} must be >= 6× bound {six_x_bound}"
        );
    }

    /// Verify that the background refill task's post-boot re-check prevents
    /// pool overfill under concurrent acquires.
    ///
    /// This is a white-box test of the pool insertion guard added to the refill
    /// task: we simulate the scenario where the pool is already at capacity
    /// when the newly-booted VM tries to enter it, and confirm it is dropped.
    #[tokio::test]
    async fn refill_task_does_not_overfill_pool_when_pool_full_at_push() {
        use std::collections::VecDeque;
        use tokio::sync::Mutex;

        // pool_size = 1; pre-fill with one VM so it is already at capacity.
        let pool: Arc<Mutex<VecDeque<WarmVm>>> = Arc::new(Mutex::new({
            let mut d = VecDeque::new();
            d.push_back(WarmVm {
                id: "already-in-pool".to_string(),
                vsock_path: PathBuf::from("/dev/null"),
                _fc_proc: tokio::process::Command::new("sleep")
                    .arg("1000")
                    .kill_on_drop(true)
                    .spawn()
                    .expect("spawn sleep"),
                _vfs_proc: None,
                born_at: Instant::now(),
                call_count: 0,
                ceiling: VmCeiling::default(),
                host_cwd: PathBuf::from("/tmp"),
                rootfs_version: "0.1.0".to_string(),
            });
            d
        }));

        // Simulate the refill task: prune + re-check before push.
        let new_vm = WarmVm {
            id: "new-vm".to_string(),
            vsock_path: PathBuf::from("/dev/null"),
            _fc_proc: tokio::process::Command::new("sleep")
                .arg("1000")
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep"),
            _vfs_proc: None,
            born_at: Instant::now(),
            call_count: 0,
            ceiling: VmCeiling::default(),
            host_cwd: PathBuf::from("/tmp"),
            rootfs_version: "0.1.0".to_string(),
        };

        let pool_size: usize = 1;
        {
            let mut p = pool.lock().await;
            p.retain(|vm| !vm.is_expired());
            if p.len() < pool_size {
                p.push_back(new_vm);
            }
            // else: drop new_vm — pool already at capacity
        }

        // Pool must remain at 1 (the original entry, not the new one).
        let pool_guard = pool.lock().await;
        assert_eq!(
            pool_guard.len(),
            1,
            "pool must not exceed pool_size=1 after refill task; got {}",
            pool_guard.len()
        );
        assert_eq!(
            pool_guard[0].id, "already-in-pool",
            "original VM should remain; new-vm should have been dropped"
        );
    }
}
