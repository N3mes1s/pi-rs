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
    CallLimits, ProbeCheck, ProbeReport, VmCeiling, VmExecution, VmSpec,
};
use crate::microvm::launcher::{MicroVmLauncher, VmHandle};
use crate::provider::SandboxError;

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
                "pi/sandbox/rootfs/{}/rootfs.img.zst",
                pi_sandbox_rootfs::ROOTFS_VERSION
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
    fn resolved_virtiofsd(&self) -> Option<PathBuf> {
        if let Some(p) = &self.virtiofsd_bin {
            return Some(p.clone());
        }
        which::which("virtiofsd").ok()
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

        // 3. vsock kernel module loaded
        let vsock_ok = std::path::Path::new("/sys/module/vsock").exists()
            || std::path::Path::new("/sys/module/vhost_vsock").exists();
        if !vsock_ok {
            blockers.push("vsock kernel module not loaded".into());
            remediation.push("sudo modprobe vsock vhost-vsock".into());
        }
        checks.push(ProbeCheck {
            name: "vsock_module",
            passed: vsock_ok,
            detail: if vsock_ok {
                Some("/sys/module/vsock or vhost_vsock present".into())
            } else {
                Some("neither /sys/module/vsock nor vhost_vsock found".into())
            },
        });

        // 4. virtiofsd binary on PATH
        let vfs_path = self.config.resolved_virtiofsd();
        let vfs_ok = vfs_path.is_some();
        if !vfs_ok {
            blockers.push("virtiofsd binary not found on $PATH".into());
            remediation
                .push("Install virtiofsd: https://gitlab.com/virtio-fs/virtiofsd".into());
        }
        checks.push(ProbeCheck {
            name: "virtiofsd_binary",
            passed: vfs_ok,
            detail: vfs_path.map(|p| p.display().to_string()),
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

        // Try to pop a warm VM that matches the ceiling and host_cwd.
        let warm_vm = {
            let mut pool = self.pool.lock().await;
            let pos = pool.iter().position(|vm| {
                !vm.is_expired()
                    && vm.ceiling == spec.vm_ceiling
                    && vm.host_cwd == spec.host_cwd
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
        let pool_clone = Arc::clone(&self.pool);
        let config_clone = Arc::clone(&self.config);
        let spec_clone = spec.clone();
        let pool_size = self.config.pool_size;
        tokio::spawn(async move {
            let current = pool_clone.lock().await.len();
            if current < pool_size {
                match cold_boot(&config_clone, &spec_clone).await {
                    Ok(new_vm) => pool_clone.lock().await.push_back(new_vm),
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
            pool: Arc::clone(&self.pool),
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
    pool: Arc<Mutex<VecDeque<WarmVm>>>,
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
        let resp = tokio::time::timeout(limits.wall_timeout + Duration::from_secs(5), async {
            let mut reader = BufReader::new(&mut stream);
            framing::read_response_with_max(
                &mut reader,
                pi_sandbox_protocol::framing::DEFAULT_MAX_LINE_BYTES,
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

        // Return to pool.
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
        };
        self.pool.lock().await.push_back(warm);
        debug!(id = %vm_id, "VM returned to pool");
        Ok(())
    }
}

// ── Cold boot ───────────────────────────────────────────────────────────────

/// Spawn a fresh Firecracker VM for the given spec and wait until the
/// guest vsock worker is ready to accept connections.
async fn cold_boot(config: &FirecrackerConfig, spec: &VmSpec) -> Result<WarmVm, SandboxError> {
    let vm_id = Uuid::new_v4().to_string();
    let run_dir = config.run_dir.join(&vm_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    let api_sock = run_dir.join("api.sock");
    let vsock_sock = run_dir.join("vsock.sock");
    let config_path = run_dir.join("fc-config.json");
    let virtiofs_sock = run_dir.join("virtiofs.sock");

    let kernel_path = config.resolved_kernel_path();
    let rootfs_path = config.resolved_rootfs_path();
    let fc_bin = config
        .resolved_firecracker()
        .ok_or_else(|| SandboxError::Unavailable("firecracker binary not found".into()))?;
    let vfs_bin = config.resolved_virtiofsd();

    // CID must be unique per-VM. Use a hash of the vm_id UUID for uniqueness
    // in range [3, 2^32-1] (0=hypervisor, 1=reserved, 2=host).
    let cid = vm_id_to_cid(&vm_id);

    // Spawn virtiofsd first (it must be ready before Firecracker boots).
    let vfs_proc = if let Some(ref vfs) = vfs_bin {
        let vfs_socket_path = virtiofs_sock.display().to_string();
        let shared_dir = spec.host_cwd.display().to_string();
        let proc = tokio::process::Command::new(vfs)
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
        // Give virtiofsd a moment to create its socket.
        tokio::time::sleep(Duration::from_millis(100)).await;
        Some(proc)
    } else {
        None
    };

    // Write Firecracker config JSON.
    let fc_config = build_fc_config(
        &kernel_path,
        &rootfs_path,
        &vsock_sock,
        cid,
        vfs_proc.as_ref().map(|_| virtiofs_sock.as_path()),
        spec,
    );
    let config_json = serde_json::to_string_pretty(&fc_config)
        .map_err(|e| SandboxError::Provider(e.to_string()))?;
    tokio::fs::write(&config_path, config_json).await?;

    // Spawn Firecracker.
    let fc_proc = tokio::process::Command::new(&fc_bin)
        .arg("--api-sock")
        .arg(&api_sock)
        .arg("--config-file")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    // Wait for the guest vsock worker to be reachable.
    wait_for_vsock_ready(&vsock_sock, cid).await?;

    Ok(WarmVm {
        id: vm_id,
        vsock_path: vsock_sock,
        _fc_proc: fc_proc,
        _vfs_proc: vfs_proc,
        born_at: Instant::now(),
        call_count: 0,
        ceiling: spec.vm_ceiling,
        host_cwd: spec.host_cwd.clone(),
    })
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
fn build_fc_config(
    kernel_path: &std::path::Path,
    rootfs_path: &std::path::Path,
    vsock_sock: &std::path::Path,
    cid: u32,
    virtiofs_sock: Option<&std::path::Path>,
    spec: &VmSpec,
) -> serde_json::Value {
    let mut config = serde_json::json!({
        "boot-source": {
            "kernel_image_path": kernel_path.display().to_string(),
            "boot_args": format!(
                "console=ttyS0 reboot=k panic=1 pci=off nomodules \
                 i8042.nokbd i8042.noaux \
                 pi.proto_version={}",
                CURRENT_PROTOCOL_VERSION
            )
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
        }
    });

    // virtio-fs share if virtiofsd is available.
    if let Some(vfs_sock) = virtiofs_sock {
        config["fs"] = serde_json::json!([
            {
                "fs_id": "work",
                "host_path": vfs_sock.display().to_string(),
                "guest_mount_point": "/work",
                "rate_limiter": null,
                "tag": "work"
            }
        ]);
    }

    config
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
}
