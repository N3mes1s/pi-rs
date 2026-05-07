//! pi-sandbox-worker — guest-side process for pi-rs microVM
//! sandbox. Runs inside an alpine guest, listens on vsock,
//! dispatches tool calls from the host into pi-tools-core,
//! writes responses back.
//!
//! Cross-platform NOTE: this binary runs only on Linux (vsock
//! is Linux-specific). On macOS/Windows it compiles to a stub
//! that exits with a clear error so workspace `cargo build`
//! still succeeds.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "pi-sandbox-worker", version)]
struct Cli {
    /// vsock port to listen on. Default matches
    /// pi_sandbox_protocol::VSOCK_DEFAULT_PORT.
    #[arg(long = "vsock-port", default_value_t = pi_sandbox_protocol::VSOCK_DEFAULT_PORT)]
    vsock_port: u32,
    /// Working directory for tools. Defaults to /work (the
    /// virtio-fs mount of the host's session cwd).
    #[arg(long = "work-dir", default_value = "/work")]
    work_dir: std::path::PathBuf,
    /// Optional log level (off, error, warn, info, debug, trace).
    #[arg(long = "log-level", default_value = "info")]
    log_level: String,
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log_level);

    // Sandbox hardening for tool subprocesses (RFD 0023 §6
    // "Bash-can't-bypass"). Both env vars are read by
    // `pi_tools_core::bash`'s `pre_exec` hook, so they apply to
    // every bash subprocess this worker spawns. The worker itself
    // is unaffected.
    //
    // PI_SANDBOX_BASH_DROP_PRIV=1
    //   setgroups(0,NULL) + setgid(1001 /pi-tool/) + setuid(1001) before
    //   exec. Bash runs as an unprivileged UID, can't read/modify
    //   worker memory, can't signal the worker, can't write to
    //   root-owned files.
    //
    // PI_SANDBOX_BASH_SECCOMP=1
    //   Pure Rust deny-list filter blocking socket(AF_VSOCK|...),
    //   mount/pivot_root/bpf/etc. Closes the bash → vsock(2,5003)
    //   policy-bypass route the seccomp commit added.
    // RW-mount-demo escape hatch: when the host launcher passes
    // `pi.bash_drop_priv=0` on the kernel cmdline, run bash as
    // root in the guest. Trade-off: loses the RFD 0023 §6 Layer 1
    // "bash can't bypass" UID separation. Required today for the
    // contextfs RW /work demo because contextfsd's FUSE bridge
    // stamps inode 1 with `0755 root:root` regardless of the
    // host directory's mode (Caps::owner_passthrough is not
    // exposed for the remote-fs backend yet — flagged upstream).
    // Default remains drop-priv; only set to 0 in the dedicated
    // RW integration test.
    let drop_priv = read_cmdline_kv("pi.bash_drop_priv").as_deref() != Some("0");
    if drop_priv {
        std::env::set_var("PI_SANDBOX_BASH_DROP_PRIV", "1");
    } else {
        eprintln!(
            "WARN: pi.bash_drop_priv=0 on kernel cmdline — bash runs as root \
             in guest (RFD 0023 §6 Layer 1 disabled). Demo / RW-mount path only."
        );
    }
    std::env::set_var("PI_SANDBOX_BASH_SECCOMP", "1");

    pi_sandbox_worker::listener::serve(cli.vsock_port, cli.work_dir).await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "pi-sandbox-worker is Linux-only (it runs inside a microVM guest with vsock support). \
         This binary cannot be executed on the current host OS — it builds here only for \
         workspace compatibility. Run from inside an alpine guest under Firecracker / vfkit / \
         cloud-hypervisor."
    );
    std::process::exit(2);
}

/// Read a `key=value` token from `/proc/cmdline`. Returns `None`
/// when the key is absent or the cmdline isn't readable.
#[cfg(target_os = "linux")]
fn read_cmdline_kv(key: &str) -> Option<String> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").ok()?;
    let needle = format!("{key}=");
    cmdline
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(&needle).map(str::to_string))
}

fn init_tracing(level: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
