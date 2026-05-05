#!/usr/bin/env bash
# crates/pi-sandbox-rootfs/build.sh
#
# Produce pi-sandbox-rootfs-vX.Y.Z.img.zst from:
#   - alpine miniroot tarball (downloaded if missing)
#   - the cargo-built pi-sandbox-worker (static-musl)
#   - a tiny init script (this file)
#
# Maintainer-only. Run on a Linux host with the following
# tools available: curl, mkfs.ext4 (e2fsprogs >= 1.42.4 for
# the unprivileged `-d <directory>` form), zstd, tar, cargo.
#
# Usage:
#   bash crates/pi-sandbox-rootfs/build.sh [VERSION]
# where VERSION defaults to ROOTFS_VERSION in src/lib.rs.
#
# Optional env vars:
#   PI_SANDBOX_KERNEL_MODULES_DIR - path to /lib/modules/<kver> directory
#     containing host kernel modules. If set and the directory exists, the
#     vsock + overlayfs modules are decompressed and bundled into the rootfs
#     at /lib/modules/<kver>/ so init can insmod them at boot. Required
#     when using a generic kernel (e.g. Ubuntu 6.8.x) where these are .ko
#     modules rather than built-in. Firecracker-optimised kernels have them
#     built-in; no bundling needed.

set -euo pipefail

VERSION="${1:-0.1.0}"
ARCH="x86_64"
ALPINE_VERSION="3.19"
ALPINE_MINI="alpine-minirootfs-${ALPINE_VERSION}.0-${ARCH}.tar.gz"
ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VERSION}/releases/${ARCH}/${ALPINE_MINI}"
WORK="$(mktemp -d -t pi-sandbox-rootfs.XXXXXX)"
OUT_DIR="$(pwd)/target/pi-sandbox-rootfs"
OUT_IMG="${OUT_DIR}/pi-sandbox-rootfs-v${VERSION}.img"
OUT_ZSTD="${OUT_IMG}.zst"

mkdir -p "${OUT_DIR}"
trap 'rm -rf "${WORK}"' EXIT

echo "==> Building pi-sandbox-rootfs v${VERSION} into ${OUT_ZSTD}"
echo "==> Working dir: ${WORK}"

# 1. Cross-compile pi-sandbox-worker + the vsock probe (used by the
# seccomp regression test) for static-musl.
echo "==> Cross-compiling pi-sandbox-worker (release, x86_64-unknown-linux-musl)"
cargo build --release --target x86_64-unknown-linux-musl --bin pi-sandbox-worker --bin pi-vsock-probe
WORKER_BIN="$(pwd)/target/x86_64-unknown-linux-musl/release/pi-sandbox-worker"
PROBE_BIN="$(pwd)/target/x86_64-unknown-linux-musl/release/pi-vsock-probe"
[[ -x "${WORKER_BIN}" ]] || { echo "ERROR: worker not built at ${WORKER_BIN}"; exit 2; }
[[ -x "${PROBE_BIN}" ]] || { echo "ERROR: probe not built at ${PROBE_BIN}"; exit 2; }

# 2. Fetch alpine miniroot if missing.
mkdir -p "${OUT_DIR}/cache"
if [[ ! -f "${OUT_DIR}/cache/${ALPINE_MINI}" ]]; then
  echo "==> Downloading ${ALPINE_URL}"
  curl --fail --location -o "${OUT_DIR}/cache/${ALPINE_MINI}" "${ALPINE_URL}"
fi

# 3. Stage rootfs.
ROOT="${WORK}/root"
mkdir -p "${ROOT}"
echo "==> Extracting alpine miniroot"
tar -xzf "${OUT_DIR}/cache/${ALPINE_MINI}" -C "${ROOT}"

# 4. Drop the worker (and the vsock probe used by tests) in /usr/local/bin.
install -m 0755 "${WORKER_BIN}" "${ROOT}/usr/local/bin/pi-sandbox-worker"
install -m 0755 "${PROBE_BIN}" "${ROOT}/usr/local/bin/pi-vsock-probe"

# 4b. UID separation (RFD 0023 §6 "Bash-can't-bypass" Layer 1).
#     pi-worker (UID 1000) runs the worker process; pi-tool (UID 1001)
#     runs every tool subprocess (bash, future tools). Bash can't read
#     pi-worker's memory, signal it, or scribble on worker-owned files.
#
#     We APPEND minimal /etc/passwd + /etc/group entries here at build
#     time. The chown of /opt /home/pi-tool /var/tmp is deferred to the
#     guest init script (runs as root inside the guest, so chown works
#     regardless of who built the rootfs).
echo 'pi-worker:x:1000:1000:pi sandbox worker:/var/empty:/sbin/nologin' >> "${ROOT}/etc/passwd"
echo 'pi-tool:x:1001:1001:pi sandbox tool subprocess:/home/pi-tool:/bin/sh'  >> "${ROOT}/etc/passwd"
echo 'pi-worker:x:1000:' >> "${ROOT}/etc/group"
echo 'pi-tool:x:1001:'   >> "${ROOT}/etc/group"
mkdir -p "${ROOT}/home/pi-tool" "${ROOT}/opt"

# 5. Write the init script. Mounts /proc, /sys; checks
#    /proc/cmdline for the proto_version pin; sets up overlay-on-tmpfs
#    root + (optional) network from cmdline pi.net.* knobs; execs the
#    worker. NOTE: /work is NOT mounted in v1 — Firecracker silently
#    drops its `fs` device config block (upstream issue #1180), so
#    the host_cwd→/work share is deferred to Commit G3 via contextfs.
mkdir -p "${ROOT}/sbin"

# Determine kernel version for modules (if PI_SANDBOX_KERNEL_MODULES_DIR is set).
BUNDLED_KVER=""
BUNDLE_MODULES=0
if [[ -n "${PI_SANDBOX_KERNEL_MODULES_DIR:-}" ]] && [[ -d "${PI_SANDBOX_KERNEL_MODULES_DIR}" ]]; then
  BUNDLED_KVER="$(basename "${PI_SANDBOX_KERNEL_MODULES_DIR}")"
  # virtiofs.ko intentionally NOT bundled — Firecracker drops the
  # `fs` device anyway (issue #1180). Re-add when contextfs lands
  # in Commit G3 if its in-guest client needs it.
  MODULES_TO_BUNDLE=(
    "kernel/net/vmw_vsock/vsock.ko.zst"
    "kernel/net/vmw_vsock/vmw_vsock_virtio_transport_common.ko.zst"
    "kernel/net/vmw_vsock/vmw_vsock_virtio_transport.ko.zst"
    "kernel/fs/overlayfs/overlay.ko.zst"
  )
  DEST_MODS="${ROOT}/lib/modules/${BUNDLED_KVER}"
  mkdir -p "${DEST_MODS}"
  FOUND_ANY=0
  for MOD in "${MODULES_TO_BUNDLE[@]}"; do
    SRC="${PI_SANDBOX_KERNEL_MODULES_DIR}/${MOD}"
    KO="${MOD%.zst}"
    DEST_DIR="${DEST_MODS}/$(dirname "${MOD}")"
    mkdir -p "${DEST_DIR}"
    if [[ -f "${SRC}" ]]; then
      # Decompress .ko.zst to .ko so busybox insmod can load it directly.
      zstd -d -o "${DEST_DIR}/$(basename "${KO}")" "${SRC}" 2>/dev/null
      FOUND_ANY=1
      echo "    bundled: ${KO}"
    fi
  done
  [[ "${FOUND_ANY}" -eq 1 ]] && BUNDLE_MODULES=1
fi

if [[ "${BUNDLE_MODULES}" -eq 1 ]]; then
cat > "${ROOT}/init" << INIT_EOF
#!/bin/sh
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
# Load vsock + overlayfs kernel modules if bundled (needed when they are
# not built-in to the kernel, e.g. Ubuntu 6.8.x generic kernels). We do
# this BEFORE pivot_root because /lib/modules lives on the rootfs lower.
MODS_DIR="/lib/modules/${BUNDLED_KVER}"
if [ -d "\$MODS_DIR" ]; then
  for m in vsock vmw_vsock_virtio_transport_common vmw_vsock_virtio_transport overlay; do
    ko=\$(find "\$MODS_DIR" -name "\${m}.ko" 2>/dev/null | head -1)
    [ -n "\$ko" ] && insmod "\$ko" 2>/dev/null || true
  done
fi
# Overlay setup: every path becomes writable. lower=current rootfs (RO at
# the VMM-drive layer because warm-pool VMs share one rootfs.img file —
# ext4 isn't a cluster fs); upper=tmpfs in guest RAM, ephemeral per-VM.
# State leaks across tool calls within ONE warm VM (no per-call reset
# yet; RFD §"Post-call hygiene" is the eventual fix). Size driven by
# the host's VmCeiling.disk_mib via pi.overlay.size_mib= cmdline; 256
# MiB is the floor when cmdline is missing/invalid.
# /run itself is on the RO rootfs; mount a tmpfs there first so we can
# build the overlay scaffolding under it.
overlay_size=\$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.overlay\.size_mib=//p')
case "\$overlay_size" in ''|*[!0-9]*) overlay_size=256 ;; esac
mount -t tmpfs -o size=8m tmpfs /run
mkdir -p /run/overlay
mount -t tmpfs -o size=\${overlay_size}m tmpfs /run/overlay
mkdir -p /run/overlay/upper /run/overlay/work /run/overlay/newroot
if mount -t overlay -o lowerdir=/,upperdir=/run/overlay/upper,workdir=/run/overlay/work overlay /run/overlay/newroot; then
  # Carry critical mounts across pivot_root + create the put_old dir.
  mkdir -p /run/overlay/newroot/proc /run/overlay/newroot/sys /run/overlay/newroot/dev /run/overlay/newroot/old_root
  mount --move /proc /run/overlay/newroot/proc
  mount --move /sys  /run/overlay/newroot/sys
  mount -t devtmpfs none /run/overlay/newroot/dev || true
  cd /run/overlay/newroot
  pivot_root . ./old_root
  exec /sbin/init-overlayed
fi
# Fallback path if overlay setup fails: keep the legacy per-path tmpfs
# mounts so the VM still boots and bash has SOMETHING writable. The
# guest will log to serial but not abort.
echo "WARN: overlay setup failed; falling back to per-path tmpfs"
mount -t tmpfs -o size=64m,mode=1777   tmpfs /tmp  2>/dev/null
mount -t tmpfs -o size=16m,mode=0700   tmpfs /root 2>/dev/null
mount -t tmpfs -o size=8m,mode=0755    tmpfs /run  2>/dev/null
mount -t tmpfs -o size=16m,mode=0755   tmpfs /var  2>/dev/null
expected_proto=1
cmdline_proto=\$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.proto_version=//p')
if [ -n "\$cmdline_proto" ] && [ "\$cmdline_proto" != "\$expected_proto" ]; then
  echo "FATAL: proto_version mismatch (expected \$expected_proto, kernel cmdline says \$cmdline_proto)"
  echo b > /proc/sysrq-trigger
  exit 1
fi
exec /usr/local/bin/pi-sandbox-worker --vsock-port=5001 --work-dir=/tmp
INIT_EOF
else
cat > "${ROOT}/init" <<'INIT_EOF'
#!/bin/sh
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
# Overlay setup (no bundled modules — overlay must be built-in or
# already loaded). lower=rootfs RO, upper=tmpfs (256 MiB cap).
mkdir -p /run/overlay
mount -t tmpfs -o size=256m tmpfs /run/overlay 2>/dev/null
mkdir -p /run/overlay/upper /run/overlay/work /run/overlay/newroot
if mount -t overlay -o lowerdir=/,upperdir=/run/overlay/upper,workdir=/run/overlay/work overlay /run/overlay/newroot; then
  mkdir -p /run/overlay/newroot/proc /run/overlay/newroot/sys /run/overlay/newroot/dev
  mount --move /proc /run/overlay/newroot/proc 2>/dev/null
  mount --move /sys  /run/overlay/newroot/sys  2>/dev/null
  mount -t devtmpfs none /run/overlay/newroot/dev 2>/dev/null || true
  cd /run/overlay/newroot
  pivot_root . old_root
  exec /sbin/init-overlayed
fi
echo "WARN: overlay setup failed; falling back to per-path tmpfs"
mount -t tmpfs -o size=64m,mode=1777   tmpfs /tmp  2>/dev/null
mount -t tmpfs -o size=16m,mode=0700   tmpfs /root 2>/dev/null
mount -t tmpfs -o size=8m,mode=0755    tmpfs /run  2>/dev/null
mount -t tmpfs -o size=16m,mode=0755   tmpfs /var  2>/dev/null
expected_proto=1
cmdline_proto=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.proto_version=//p')
if [ -n "$cmdline_proto" ] && [ "$cmdline_proto" != "$expected_proto" ]; then
  echo "FATAL: proto_version mismatch (expected $expected_proto, kernel cmdline says $cmdline_proto)"
  echo b > /proc/sysrq-trigger
  exit 1
fi
exec /usr/local/bin/pi-sandbox-worker --vsock-port=5001 --work-dir=/tmp
INIT_EOF
fi
chmod 0755 "${ROOT}/init"

# Post-pivot init runs INSIDE the overlay'd root (after pivot_root succeeds
# in /init). Idempotently writes this once for both BUNDLE_MODULES branches.
cat > "${ROOT}/sbin/init-overlayed" << 'POST_EOF'
#!/bin/sh
# Lazy-umount the old root so its tmpfs upper releases when references drop.
umount -l /old_root 2>/dev/null
rmdir /old_root 2>/dev/null
# /work is intentionally not mounted in v1 — Firecracker silently
# drops the virtio-fs `fs` device (upstream issue #1180). The
# host_cwd→/work share returns under contextfs in Commit G3.

# Optional eth0 setup. The host injects `pi.net.ip=<cidr>`,
# `pi.net.gw=<gw>`, `pi.net.dns=<csv>` on the kernel cmdline when the
# launcher's NetworkPolicy is `Allow`. When absent (default `Deny`),
# eth0 stays down and no /etc/resolv.conf is written.
#
# The host-side wiring (pasta, nftables, unprivileged userns) is
# documented in crates/pi-sandbox/docs/NETWORKING.md.
net_ip=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.net\.ip=//p')
net_gw=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.net\.gw=//p')
net_dns=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.net\.dns=//p')
if [ -n "$net_ip" ] && [ -n "$net_gw" ]; then
  ip link set eth0 up 2>/dev/null
  ip addr add "$net_ip" dev eth0 2>/dev/null
  ip route add default via "$net_gw" 2>/dev/null
  if [ -n "$net_dns" ]; then
    : > /etc/resolv.conf
    for ns in $(echo "$net_dns" | tr ',' ' '); do
      echo "nameserver $ns" >> /etc/resolv.conf
    done
  fi
fi

expected_proto=1
cmdline_proto=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.proto_version=//p')
if [ -n "$cmdline_proto" ] && [ "$cmdline_proto" != "$expected_proto" ]; then
  echo "FATAL: proto_version mismatch (expected $expected_proto, kernel cmdline says $cmdline_proto)"
  echo b > /proc/sysrq-trigger
  exit 1
fi

# UID-separation prep (RFD 0023 §6 Layer 1). /etc/passwd entries for
# pi-worker (1000) and pi-tool (1001) were appended at build time;
# we chown the persistent-scratch dirs at boot, when we're root in
# the overlay (build-time chown failed because the rootfs builder
# isn't root). After this, bash subprocesses dropping to UID 1001
# can write to /opt + /home/pi-tool + /var/tmp.
mkdir -p /home/pi-tool /opt /var/tmp /tmp 2>/dev/null
chown 1001:1001 /home/pi-tool 2>/dev/null
chmod 0700 /home/pi-tool 2>/dev/null
chown 1001:1001 /opt 2>/dev/null
chmod 0775 /opt 2>/dev/null
# /tmp + /var/tmp world-writable with sticky bit (Linux convention).
chmod 1777 /tmp 2>/dev/null
chmod 1777 /var/tmp 2>/dev/null

exec /usr/local/bin/pi-sandbox-worker --vsock-port=5001 --work-dir=/tmp
POST_EOF
chmod 0755 "${ROOT}/sbin/init-overlayed"

# Pre-create /work as a mount point (it must exist on the read-only rootfs
# so the virtiofs mount succeeds without needing a writable root).
mkdir -p "${ROOT}/work"

# Create a /bin/bash wrapper that delegates to /bin/sh. BashTool (pi-tools-core)
# runs `bash -lc`, but Alpine only has /bin/sh (busybox ash). A wrapper script
# (not a symlink — busybox would interpret the applet name) passes all args through.
printf '#!/bin/sh\nexec /bin/sh "$@"\n' > "${ROOT}/bin/bash"
chmod 0755 "${ROOT}/bin/bash"

# 6. Stamp the version + protocol marker so the host can verify.
mkdir -p "${ROOT}/etc"
echo "${VERSION}" > "${ROOT}/etc/pi-sandbox-version"
echo "1" > "${ROOT}/etc/pi-sandbox-proto-version"

# 7. Build the ext4 image with -d for unprivileged construction.
echo "==> Creating ext4 image"
SIZE_MIB=$((80))
truncate -s "${SIZE_MIB}M" "${OUT_IMG}"
mkfs.ext4 -F -L pi-sandbox-rootfs -d "${ROOT}" "${OUT_IMG}" >/dev/null

# 8. Compress with zstd.
echo "==> Compressing with zstd"
rm -f "${OUT_ZSTD}"
zstd --ultra -22 -o "${OUT_ZSTD}" "${OUT_IMG}"

# 9. Sha256 + size.
SHA="$(sha256sum "${OUT_ZSTD}" | awk '{print $1}')"
SZ="$(stat -c%s "${OUT_ZSTD}")"

# 10. Print the manifest line the maintainer pastes into the
#     pi-sandbox crate's compile-time constants.
cat <<MANIFEST_EOF

==> SUCCESS
Output: ${OUT_ZSTD}
Size  : ${SZ} bytes
SHA256: ${SHA}

Embed in pi-sandbox::cache::ROOTFS_MANIFEST:
   ROOTFS_VERSION       = "${VERSION}"
   ROOTFS_URL           = "https://github.com/pi-rs/releases/download/sandbox-rootfs-v${VERSION}/pi-sandbox-rootfs-v${VERSION}.img.zst"
   ROOTFS_SHA256        = "${SHA}"
   ROOTFS_SIZE_BYTES    = ${SZ}

MANIFEST_EOF
