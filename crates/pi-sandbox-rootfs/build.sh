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
#     virtiofs + vsock modules are decompressed and bundled into the rootfs
#     at /lib/modules/<kver>/ so init can insmod them at boot.
#     Required when using a generic kernel (e.g. Ubuntu 6.8.x) where
#     virtiofs and vsock are .ko modules rather than built-in.
#     Firecracker-optimised kernels have them built-in; no bundling needed.

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

# 1. Cross-compile pi-sandbox-worker for static-musl.
echo "==> Cross-compiling pi-sandbox-worker (release, x86_64-unknown-linux-musl)"
cargo build --release --target x86_64-unknown-linux-musl --bin pi-sandbox-worker
WORKER_BIN="$(pwd)/target/x86_64-unknown-linux-musl/release/pi-sandbox-worker"
[[ -x "${WORKER_BIN}" ]] || { echo "ERROR: worker not built at ${WORKER_BIN}"; exit 2; }

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

# 4. Drop the worker in /usr/local/bin.
install -m 0755 "${WORKER_BIN}" "${ROOT}/usr/local/bin/pi-sandbox-worker"

# 5. Write the init script. Mounts /proc, /sys, /work; checks
#    /proc/cmdline for the proto_version pin; execs the worker.
#    virtiofs is attempted but NOT fatal if unavailable (e.g. when
#    the Firecracker build was compiled without virtiofs support).
#    The worker always starts; tools needing /work will fail gracefully.
mkdir -p "${ROOT}/sbin"

# Determine kernel version for modules (if PI_SANDBOX_KERNEL_MODULES_DIR is set).
BUNDLED_KVER=""
BUNDLE_MODULES=0
if [[ -n "${PI_SANDBOX_KERNEL_MODULES_DIR:-}" ]] && [[ -d "${PI_SANDBOX_KERNEL_MODULES_DIR}" ]]; then
  BUNDLED_KVER="$(basename "${PI_SANDBOX_KERNEL_MODULES_DIR}")"
  MODULES_TO_BUNDLE=(
    "kernel/net/vmw_vsock/vsock.ko.zst"
    "kernel/net/vmw_vsock/vmw_vsock_virtio_transport_common.ko.zst"
    "kernel/net/vmw_vsock/vmw_vsock_virtio_transport.ko.zst"
    "kernel/fs/fuse/virtiofs.ko.zst"
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
# Load vsock + virtiofs kernel modules if bundled (needed when they are
# not built-in to the kernel, e.g. Ubuntu 6.8.x generic kernels).
MODS_DIR="/lib/modules/${BUNDLED_KVER}"
if [ -d "\$MODS_DIR" ]; then
  for m in vsock vmw_vsock_virtio_transport_common vmw_vsock_virtio_transport virtiofs; do
    ko=\$(find "\$MODS_DIR" -name "\${m}.ko" 2>/dev/null | head -1)
    [ -n "\$ko" ] && insmod "\$ko" 2>/dev/null || true
  done
fi
# /work is the virtio-fs mount point. Attempt mount but do not abort if it
# fails: the worker starts either way and reports an error per failing call.
mkdir -p /work 2>/dev/null || true
mount -t virtiofs work /work 2>/dev/null || echo "WARN: virtiofs mount skipped (not available)"
expected_proto=1
cmdline_proto=\$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.proto_version=//p')
if [ -n "\$cmdline_proto" ] && [ "\$cmdline_proto" != "\$expected_proto" ]; then
  echo "FATAL: proto_version mismatch (expected \$expected_proto, kernel cmdline says \$cmdline_proto)"
  echo b > /proc/sysrq-trigger
  exit 1
fi
exec /usr/local/bin/pi-sandbox-worker --vsock-port=5001 --work-dir=/work
INIT_EOF
else
cat > "${ROOT}/init" <<'INIT_EOF'
#!/bin/sh
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
# /work is the virtio-fs mount point. Attempt mount but do not abort if it
# fails: the worker starts either way and reports an error per failing call.
mkdir -p /work 2>/dev/null || true
mount -t virtiofs work /work 2>/dev/null || echo "WARN: virtiofs mount skipped (not available)"
expected_proto=1
cmdline_proto=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.proto_version=//p')
if [ -n "$cmdline_proto" ] && [ "$cmdline_proto" != "$expected_proto" ]; then
  echo "FATAL: proto_version mismatch (expected $expected_proto, kernel cmdline says $cmdline_proto)"
  echo b > /proc/sysrq-trigger
  exit 1
fi
exec /usr/local/bin/pi-sandbox-worker --vsock-port=5001 --work-dir=/work
INIT_EOF
fi
chmod 0755 "${ROOT}/init"

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
