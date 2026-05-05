//! Linux seccomp filter for `bash` tool subprocesses, installed
//! between fork and exec via `pre_exec`.
//!
//! Threat: a prompt-injected `bash` payload running inside the
//! microvm guest can otherwise call `socket(AF_VSOCK, ...)` and
//! `connect(host_cid=2, port=5003)` directly, reaching the host's
//! `web_search` proxy listener and bypassing every per-call audit
//! / policy gate the worker normally enforces. The same payload
//! could also `mount`, `pivot_root`, `kexec_load`, etc. to mess
//! with the sandbox boundary.
//!
//! Defense: a deny-list seccomp filter installed at fork time,
//! BEFORE bash exec's. The filter blocks:
//!
//! - `socket(AF_VSOCK | AF_PACKET, ...)` — kills the "reach the
//!   host listener" route and the "raw L2 packet" route. Each is
//!   checked on the first argument (domain). Unknown families pass.
//!   AF_NETLINK is intentionally NOT blocked: it's used by `ip`,
//!   `ss`, and other read-only network admin utilities that bash
//!   payloads legitimately call. AF_NETLINK doesn't enable
//!   exfiltration (no kernel-mediated WAN egress), and any
//!   actual netns mutation via NETLINK requires CAP_NET_ADMIN
//!   which the unprivileged pi-tool UID doesn't have.
//!
//! - filesystem-boundary syscalls: `mount`, `umount2`, `pivot_root`,
//!   `chroot`. Bash shouldn't be remounting the overlay or
//!   chrooting; if it is, that's adversarial.
//!
//! - kernel-boundary syscalls: `init_module`, `delete_module`,
//!   `finit_module`, `bpf`, `kexec_load`, `kexec_file_load`,
//!   `reboot`. Defense in depth against kernel-CVE-driven escape.
//!
//! Anything else (including ALL the syscalls bash + apk + cargo
//! actually use) passes through unchanged. This is deliberately a
//! narrow deny-list — an allow-list is safer in theory but
//! requires curating every syscall the toolchain might use across
//! every future binary the agent installs, which is unmaintainable.
//!
//! Failure modes: if the filter fails to install, we return an
//! `EPERM` error from `pre_exec`, which causes the spawn to fail.
//! That's the right behavior: a bash subprocess WITHOUT the filter
//! is precisely the threat we're defending against.
//!
//! Pure Rust via the `seccompiler` crate (Firecracker's filter
//! library) — no `libseccomp` C dep, links cleanly under
//! x86_64-unknown-linux-musl.

use std::collections::BTreeMap;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

/// Install the bash-tool seccomp filter on the current thread (in
/// the child process between fork and exec). Returns `Err(message)`
/// on any failure; the caller in `pre_exec` lifts that to `EPERM`.
pub(super) fn install_bash_filter() -> Result<(), String> {
    let filter = build_filter().map_err(|e| format!("seccomp build: {e}"))?;
    let prog: BpfProgram = filter
        .try_into()
        .map_err(|e| format!("seccomp compile: {e}"))?;
    seccompiler::apply_filter(&prog).map_err(|e| format!("seccomp apply: {e}"))?;
    Ok(())
}

fn build_filter() -> Result<SeccompFilter, seccompiler::BackendError> {
    // AF_VSOCK = 40, AF_PACKET = 17 on Linux. glibc/musl headers,
    // `bits/socket.h`. AF_NETLINK = 16 is intentionally NOT blocked
    // — see the module-doc.
    const AF_VSOCK: u64 = 40;
    const AF_PACKET: u64 = 17;

    fn deny_socket_family(family: u64) -> Result<SeccompRule, seccompiler::BackendError> {
        SeccompRule::new(vec![SeccompCondition::new(
            0, // arg index — `domain`
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            family,
        )?])
    }

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // Block these specific socket() families on x86_64 (libc::SYS_socket = 41).
    rules.insert(
        libc::SYS_socket,
        vec![
            deny_socket_family(AF_VSOCK)?,
            deny_socket_family(AF_PACKET)?,
        ],
    );

    // Block these whole syscalls regardless of args.
    let unconditional_deny = [
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_init_module,
        libc::SYS_delete_module,
        libc::SYS_finit_module,
        libc::SYS_bpf,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_reboot,
    ];
    for nr in unconditional_deny {
        rules.insert(nr, vec![]);
    }

    SeccompFilter::new(
        rules,
        // Default for syscalls NOT in the rules map: allow.
        SeccompAction::Allow,
        // Match-action for syscalls IN the rules map (with conditions
        // satisfied for socket-family rules; always for unconditional
        // ones with empty rule vec): return EPERM. Returning Errno is
        // visible to the caller as a normal error and lets bash
        // produce a meaningful "Operation not permitted" — better UX
        // than KillProcess for adversary-debugging during dogfood.
        SeccompAction::Errno(libc::EPERM as u32),
        target_arch(),
    )
}

#[cfg(target_arch = "x86_64")]
fn target_arch() -> TargetArch {
    TargetArch::x86_64
}

#[cfg(target_arch = "aarch64")]
fn target_arch() -> TargetArch {
    TargetArch::aarch64
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn target_arch() -> TargetArch {
    // Other architectures aren't a microvm-supported target today
    // (the rootfs is x86_64-musl); seccompiler will return a build
    // error if we hit this path, which is loud enough.
    compile_error!("seccomp filter only supported on x86_64 and aarch64");
}
