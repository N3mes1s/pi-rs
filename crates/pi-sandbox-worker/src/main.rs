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

    // Signal to `pi_tools_core::bash::seccomp` that any bash
    // subprocess we spawn should install the seccomp deny-list
    // filter (block AF_VSOCK socket() + mount/pivot_root/bpf/etc).
    // Closes the bash → vsock(2,5003) policy-bypass route. The
    // env var is inherited by every child the worker spawns; the
    // filter itself is installed by `bash.rs` between fork and
    // exec via `Command::pre_exec`, so the worker process itself
    // is unaffected.
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

fn init_tracing(level: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
