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

/// Transport mode for the worker.
///
/// `Vsock` (default): listen for incoming connections on the given vsock port.
/// `Stdin`: one-shot mode for remote sandbox transports (E2B, RFD 0026).
///   Reads one ToolRequest from stdin, writes one ToolResponse to stdout, exits.
#[derive(clap::ValueEnum, Clone, Debug, Default)]
enum Transport {
    #[default]
    Vsock,
    Stdin,
}

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
    /// Transport mode: vsock (default, microVM path) or stdin (one-shot,
    /// for E2B remote sandbox — RFD 0026).
    #[arg(long = "transport", default_value = "vsock")]
    transport: Transport,
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
    std::env::set_var("PI_SANDBOX_BASH_DROP_PRIV", "1");
    std::env::set_var("PI_SANDBOX_BASH_SECCOMP", "1");

    match cli.transport {
        Transport::Vsock => {
            pi_sandbox_worker::listener::serve(cli.vsock_port, cli.work_dir).await
        }
        Transport::Stdin => {
            pi_sandbox_worker::listener::serve_stdio(cli.work_dir).await
        }
    }
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

fn init_tracing(level: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
