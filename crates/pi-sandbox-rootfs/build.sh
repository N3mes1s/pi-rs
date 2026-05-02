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
mkdir -p "${ROOT}/sbin"
cat > "${ROOT}/init" <<'INIT_EOF'
#!/bin/sh
mount -t proc none /proc 2>/dev/null
mount -t sysfs none /sys 2>/dev/null
mkdir -p /work
mount -t virtiofs work /work || echo "WARN: virtiofs mount of /work failed; continuing"
expected_proto=1
cmdline_proto=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.proto_version=//p')
if [ -n "$cmdline_proto" ] && [ "$cmdline_proto" != "$expected_proto" ]; then
  echo "FATAL: proto_version mismatch (expected $expected_proto, kernel cmdline says $cmdline_proto)"
  echo b > /proc/sysrq-trigger
fi
exec /usr/local/bin/pi-sandbox-worker --vsock-port=5001 --work-dir=/work
INIT_EOF
chmod 0755 "${ROOT}/init"

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
