# RFD 0023 — Known Issues from D's End-to-End Validation

- **Status:** Active blocker (2026-05-02)
- **Discovered during:** post-D validation (PR-D was reviewed-only; never executed)
- **Affects:** RFD 0023's local microVM design + the FirecrackerLauncher (Commit D) implementation

## Summary

Commit D (`FirecrackerLauncher` with warm pool) merged after 7 fix-loop iterations and 26 unit tests passing. The smoke test (`crates/pi-sandbox/tests/firecracker_smoke.rs`) is gated on `PI_SANDBOX_FC_TEST=1` plus a maintainer-provided kernel + rootfs. **Neither the implementer nor the reviewer ever ran the smoke test against a real VM.** Subsequent end-to-end validation (with built rootfs + aegis kernel + firecracker v1.15.0) revealed two blocking issues:

1. **Firecracker has no virtio-fs support** — the `fs` field in firecracker's config-JSON is silently ignored; no virtio-fs device is wired into the guest; `mount -t virtiofs work /work` fails with "No such device". RFD 0023 §3's claim that "v1.0 ships writable virtio-fs cwd" is not achievable on Firecracker.
2. **Worker fails to bind vsock listener on port 5001** — even after bypassing the /work mount, the worker exits with `Error: failed to bind vsock listener on port 5001`. Root cause: not yet diagnosed (likely missing virtio-vsock device or kernel module).

Both issues mean **D does not actually function end-to-end as merged.** The "MERGED" status reflects code-review approval, not behavioural validation.

## Issue 1: Firecracker does not support virtio-fs

### What the RFD assumed

RFD 0023 v0.4 §3 specifies:

> `/work` in the guest is a virtio-fs mount of the host's session cwd, **read-write**. Tools that mutate files (write/edit) modify host files directly through virtio-fs. The guest enforces no path traversal beyond `/work` …

The build.sh init script enforces this at boot:

```sh
mount -t virtiofs work /work || { echo "FATAL: virtiofs mount of /work failed"; exit 1; }
```

### What's actually true

Firecracker's config-JSON parser **silently accepts unknown fields**. The `fs` block in our generated config:

```json
"fs": [{ "fs_id": "work", "socket": ".../virtiofs.sock", "tag": "work" }]
```

…is silently dropped. The kernel command line shows only the drive + vsock virtio_mmio devices (two entries), not three:

```
virtio_mmio.device=4K@0xc0001000:5 virtio_mmio.device=4K@0xc0002000:6
```

Empirical confirmation: a config with a fabricated nonsense field also boots without error. Firecracker is non-strict.

Firecracker has historically rejected virtio-fs by design (https://github.com/firecracker-microvm/firecracker/issues/1180). v1.15.0's CHANGELOG and `--help` confirm no `vhost-user-fs` or `virtio-fs` support.

### The architectural consequence

**RFD 0023 cannot ship "writable virtio-fs cwd" on Firecracker.** The maintainer has three real options:

- **Option A (preferred long-term): pivot to Cloud-Hypervisor on Linux.** Cloud-Hypervisor (https://www.cloudhypervisor.org/) supports vhost-user-fs natively. RFD 0023 §4 already lists `CloudHypervisorLauncher` as the Windows path; switching Linux to it as well would unify the launcher across two of three OSes. Firecracker would either be dropped or kept as a "no-fs-share" mode for use cases where /work isn't needed (e.g. read-only inspection agents, evaluators that don't touch disk).
- **Option B: drive-based /work sharing.** Each VM acquire creates a small ext4 image, copies the host cwd in, mounts as `/dev/vdb` in the guest. After tool completion, copies back. Works with Firecracker but: (a) slower (copy in + out per session), (b) breaks RW semantics during the session (mutations to the in-guest image don't reflect to the host until copy-back), (c) more complex correctness story (partial writes, atomic flush).
- **Option C: no shared fs.** Tools that need fs access (write/edit/grep across host files) become unsupported under microvm. Pi falls back to `local-process` for those. The microvm sandbox is then only useful for `bash`-style isolated commands. This is the smallest change but eliminates the primary use case.

### Action required

**Before declaring D shippable**, RFD 0023 must be amended to:

1. Acknowledge the Firecracker virtio-fs gap explicitly.
2. Pick one of A / B / C above (or another).
3. Update the launcher's contract accordingly. If Option A: `FirecrackerLauncher` is downgraded to "no-fs-share microVM" status, and `CloudHypervisorLauncher` becomes the primary Linux launcher (RFD 0023 §4 currently picks Cloud-Hypervisor only on Windows).
4. Update `build.sh`'s init to match the chosen path. Today's "FATAL: virtiofs mount of /work failed; exit 1" is correct under Option A (pivots to Cloud-Hypervisor); needs replacement under Option B or C.

## Issue 2: Worker fails to bind vsock listener

After temporarily bypassing the /work mount fatality, the smoke test still fails:

```
Run /init as init process
WARN: virtiofs mount of /work failed; continuing
Error: failed to bind vsock listener on port 5001
```

This means the guest kernel boots, init runs, but the worker can't `bind()` on its vsock listener. Possible causes (not yet diagnosed):

- The guest kernel boots without virtio-vsock device wired up (check kernel cmdline's virtio_mmio entries).
- The vsock kernel module isn't loaded (check `/proc/modules` inside the guest).
- The guest worker's `tokio-vsock` listen is using the wrong socket family (AF_VSOCK vs something else).
- The CID assigned by the launcher conflicts with reserved values (0/1/2).

The aegis kernel does have `vsock` symbol per `strings /home/nemesis/aegis-host-temp/images/generic/kernel`, so vsock is compiled in. The bind error suggests a config-side issue, not a kernel-feature issue.

### Action required

**This must be fixed before any "MERGED" claim on D is honest.** Steps:

1. Boot the guest with `console=ttyS0` already wired (it is).
2. Patch the worker to print more detail on bind failure (`addr` + raw `errno`).
3. Verify the guest kernel has the virtio-vsock module / built-in driver active (likely the launcher needs to add `vsock_vmtransport=...` or similar to boot args).
4. Confirm the launcher's vsock config matches what the kernel expects.

## Process improvement

The validation gap (review-only PR shipped as "MERGED") happened because:

- The smoke test was gated on `PI_SANDBOX_FC_TEST=1` env var.
- The implementer didn't have the kernel + rootfs prereqs to set the gate during fix-loop iterations.
- The reviewer didn't either.
- The campaign converged on "code review green" without behavioural verification.

**Going forward**, RFD 0023's remaining commits (E vfkit, F cloud-hypervisor, G provider+CLI) must:

- Either run their gated integration tests in the campaign (provision the prereqs in the maintainer's host before launch).
- Or have the campaign explicitly assigned a **post-merge validation** milestone that boots a real VM and asserts the smoke-test outcome.

Specifically: never call something "MERGED" again when its smoke test was skipped.

## Files affected

- `crates/pi-sandbox/src/microvm/firecracker.rs` — current FirecrackerLauncher impl; needs revision per chosen Option (A/B/C).
- `crates/pi-sandbox-rootfs/build.sh` — init script's /work fatality matches Option A; revise per choice.
- `crates/pi-sandbox/tests/firecracker_smoke.rs` — the gated test that never ran during D.
- `rfd/0023-sandbox-microvm.md` — needs an amendment with the chosen Option and a "Known issues" section.

## Open questions for the maintainer

1. **Which Option (A / B / C) is the right path?** The decision shapes whether D's code stays, gets reverted, or gets repurposed.
2. **Should D be reverted from main** (it claims to work but doesn't), or kept as a "no-fs-share" stub with an Option-A successor in C/D-redux?
3. **Vsock bind failure** — is this a separate issue worth diagnosing first (might unblock a no-fs-share validation) or are we picking Option A and rebuilding D against Cloud-Hypervisor anyway?

(Notes from validation session 2026-05-02. PI_SANDBOX_FC_DEBUG=1 instrumentation was added briefly to capture firecracker stdout/stderr; reverted before this doc landed.)
