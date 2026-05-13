//! Tiny test probe: tries `socket(AF_VSOCK, SOCK_STREAM, 0)`. Exits
//! with errno on failure, 0 on success. Used by the seccomp
//! regression test to confirm bash subprocesses can't open vsock.
//!
//! Lives in pi-sandbox-worker because it's only useful inside the
//! guest rootfs (linked statically against musl by the build.sh
//! release path).

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    // AF_VSOCK = 40, SOCK_STREAM = 1.
    let fd = unsafe { libc::socket(40, 1, 0) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        let raw = e.raw_os_error().unwrap_or(0);
        eprintln!("socket failed errno={} ({})", raw, e);
        return std::process::ExitCode::from(raw.min(255) as u8);
    }
    println!("socket succeeded fd={fd} (THIS IS BAD)");
    unsafe { libc::close(fd) };
    std::process::ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pi-vsock-probe is Linux-only");
    std::process::exit(2);
}
