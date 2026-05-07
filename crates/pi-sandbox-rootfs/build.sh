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

# 1. Cross-compile pi-sandbox-worker + the vsock probe (seccomp test)
#    + the contextfs vsock bridge for static-musl.
echo "==> Cross-compiling pi-sandbox-worker + probes + cfs-vsock-bridge (release, x86_64-unknown-linux-musl)"
cargo build --release --target x86_64-unknown-linux-musl \
  --bin pi-sandbox-worker --bin pi-vsock-probe \
  --bin pi-cfs-vsock-bridge --bin pi-cfs-broker-vsock-bridge
TARGET_REL="$(pwd)/target/x86_64-unknown-linux-musl/release"
WORKER_BIN="${TARGET_REL}/pi-sandbox-worker"
PROBE_BIN="${TARGET_REL}/pi-vsock-probe"
CFS_BRIDGE_BIN="${TARGET_REL}/pi-cfs-vsock-bridge"
CFS_BROKER_BRIDGE_BIN="${TARGET_REL}/pi-cfs-broker-vsock-bridge"
[[ -x "${WORKER_BIN}" ]] || { echo "ERROR: worker not built at ${WORKER_BIN}"; exit 2; }
[[ -x "${PROBE_BIN}" ]] || { echo "ERROR: probe not built at ${PROBE_BIN}"; exit 2; }
[[ -x "${CFS_BRIDGE_BIN}" ]] || { echo "ERROR: cfs-vsock-bridge not built at ${CFS_BRIDGE_BIN}"; exit 2; }
[[ -x "${CFS_BROKER_BRIDGE_BIN}" ]] || { echo "ERROR: cfs-broker-vsock-bridge not built at ${CFS_BROKER_BRIDGE_BIN}"; exit 2; }

# 1b. Locate or build the contextfsd binary. RFD 0023 §3.5: the guest
#     runs contextfs's own daemon to mount /work via FUSE, talking to
#     the host-side cfs-fs-server over vsock(2,5005). We do NOT
#     reimplement contextfs in pi-rs.
#
#     Resolution order:
#       1. PI_SANDBOX_CONTEXTFSD_BIN env var (explicit override)
#       2. ../contextfs/target/x86_64-unknown-linux-musl/release/contextfsd
#          (built via `cargo build --release --target x86_64-unknown-linux-musl
#           --bin contextfsd` inside that workspace)
#     Fail-fast if neither resolves.
CONTEXTFSD_BIN="${PI_SANDBOX_CONTEXTFSD_BIN:-}"
if [[ -z "${CONTEXTFSD_BIN}" ]]; then
  CANDIDATE="$(pwd)/../contextfs/target/x86_64-unknown-linux-musl/release/contextfsd"
  if [[ -x "${CANDIDATE}" ]]; then
    CONTEXTFSD_BIN="${CANDIDATE}"
  fi
fi
if [[ -z "${CONTEXTFSD_BIN}" ]] || [[ ! -x "${CONTEXTFSD_BIN}" ]]; then
  echo "ERROR: contextfsd binary not found."
  echo "       Build it with:"
  echo "         (cd ../contextfs && cargo build --release --target x86_64-unknown-linux-musl --bin contextfsd)"
  echo "       or set PI_SANDBOX_CONTEXTFSD_BIN to an existing static-musl binary."
  exit 2
fi
echo "==> Using contextfsd: ${CONTEXTFSD_BIN}"

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

# 4. Drop the worker, vsock probe (tests), cfs-vsock-bridge, and
#    contextfsd in /usr/local/bin.
install -m 0755 "${WORKER_BIN}"            "${ROOT}/usr/local/bin/pi-sandbox-worker"
install -m 0755 "${PROBE_BIN}"             "${ROOT}/usr/local/bin/pi-vsock-probe"
install -m 0755 "${CFS_BRIDGE_BIN}"        "${ROOT}/usr/local/bin/pi-cfs-vsock-bridge"
install -m 0755 "${CFS_BROKER_BRIDGE_BIN}" "${ROOT}/usr/local/bin/pi-cfs-broker-vsock-bridge"
install -m 0755 "${CONTEXTFSD_BIN}"        "${ROOT}/usr/local/bin/contextfsd"

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

# RFD 0023 §3.5 (G3): contextfs `/work` mount.
#
# Topology (guest side):
#   contextfsd  --reads/writes-->  /run/cfs.sock
#                                       |
#                                pi-cfs-vsock-bridge
#                                       |
#                                vsock(2, 5005)  ---> host pi-rs
#                                                       -> cfs-fs-server (UDS)
#                                                       -> host_cwd
#
# Skipped entirely if the cmdline carries `pi.contextfs=off`. The
# host launcher omits the binaries when contextfs isn't wired up,
# so a bridge bind / contextfsd dial would silently fail and the
# warn line below is the only artifact.
contextfs_mode=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.contextfs=//p')
if [ "$contextfs_mode" != "off" ] && \
   [ -x /usr/local/bin/pi-cfs-vsock-bridge ] && \
   [ -x /usr/local/bin/contextfsd ]; then
  # Ensure /dev/fuse exists (devtmpfs creates it on most kernels;
  # belt-and-braces for kernels missing CONFIG_FUSE_FS=y where the
  # module is loadable but no node was created).
  if [ ! -c /dev/fuse ]; then
    mknod /dev/fuse c 10 229 2>/dev/null
    chmod 0666 /dev/fuse 2>/dev/null
  fi

  # Bridge listens on /run/cfs.sock, forwards to vsock(2,5005).
  # Start it FIRST so contextfsd's first dial of /run/cfs.sock
  # finds an accept()ing peer.
  mkdir -p /run /var/log /etc/contextfs /var/cache/contextfs /var/lib/contextfs
  /usr/local/bin/pi-cfs-vsock-bridge >/var/log/cfs-vsock-bridge.log 2>&1 &

  # Wait briefly for the bridge to bind /run/cfs.sock.
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -S /run/cfs.sock ] && break
    sleep 0.1
  done

  # RW mode (RFD 0023 §3.5 / Commit G3 step 3): the host launcher
  # sets pi.contextfs.rw=1 + pi.contextfs.tenant_secret_hex=<64hex>
  # on the kernel cmdline when PI_SANDBOX_CONTEXTFS_RW=1. The
  # tenant_secret_hex is decoded into /etc/contextfs/tenant-secret
  # so the daemon's audit/decision-id derivation matches the
  # host-side broker's (per contextfs embedder-broker quickstart:
  # SAME raw bytes on both sides).
  contextfs_rw=$(tr ' ' '\n' < /proc/cmdline | sed -n 's/^pi\.contextfs\.rw=//p')
  contextfs_secret_hex=$(tr ' ' '\n' < /proc/cmdline \
    | sed -n 's/^pi\.contextfs\.tenant_secret_hex=//p')

  # Tenant secret: prefer the cmdline-supplied hex (RW mode);
  # fall back to a fresh 32-byte random for RO. Mode 0600 either
  # way (the daemon's TenantSecret::from_path enforces this).
  if [ -n "$contextfs_secret_hex" ] && [ ${#contextfs_secret_hex} -eq 64 ]; then
    # Decode 64-hex into 32 raw bytes. busybox awk doesn't have
    # strtonum (gawk extension), so use sed to inject \x escapes
    # then printf — busybox printf does honour \xNN.
    escaped=$(printf '%s' "$contextfs_secret_hex" | sed 's/\(..\)/\\x\1/g')
    # shellcheck disable=SC2059  # intentional: $escaped is a printf format string of \xNN escapes
    printf "$escaped" > /etc/contextfs/tenant-secret 2>/dev/null
  elif [ ! -f /etc/contextfs/tenant-secret ]; then
    if [ -c /dev/urandom ]; then
      head -c 32 /dev/urandom > /etc/contextfs/tenant-secret 2>/dev/null
    else
      printf 'pi-sandbox-tenant-secret-vm-default-padded-32b' > /etc/contextfs/tenant-secret
    fi
  fi
  chmod 0600 /etc/contextfs/tenant-secret

  # Cedar policy. RO mode: default-permit (broker isn't running;
  # daemon's in-process PDP fallback is the gate). RW mode: the
  # host writes /etc/contextfs/policy.cedar via the broker's
  # --policy flag, but pi-rs's host writes a copy into the
  # /work-overlay-side run_dir, which the guest CAN'T see — so
  # the in-guest config still uses the default-permit shape, and
  # the broker is the authoritative gate.
  if [ ! -f /etc/contextfs/policy.cedar ]; then
    printf 'permit (principal, action, resource);\n' > /etc/contextfs/policy.cedar
  fi

  # contextfsd config. RW mode adds [broker].socket_path pointing
  # at the broker's UDS (which the broker bridge forwards to
  # vsock(2,5006)) and flips read_only=false on the mount.
  if [ ! -f /etc/contextfs/contextfsd.toml ]; then
    if [ "$contextfs_rw" = "1" ]; then
      MOUNT_RO="false"
      BROKER_BLOCK='[broker]
socket_path = "/run/contextfs/broker.sock"
'
      # Start the broker bridge (UDS /run/contextfs/broker.sock
      # ↔ vsock(2,5006)) BEFORE contextfsd's broker dial.
      if [ -x /usr/local/bin/pi-cfs-broker-vsock-bridge ]; then
        /usr/local/bin/pi-cfs-broker-vsock-bridge \
          >/var/log/cfs-broker-vsock-bridge.log 2>&1 &
        for _ in 1 2 3 4 5 6 7 8 9 10; do
          [ -S /run/contextfs/broker.sock ] && break
          sleep 0.1
        done
      fi
    else
      MOUNT_RO="true"
      BROKER_BLOCK=""
    fi
    cat > /etc/contextfs/contextfsd.toml <<CFG_EOF
tenant_secret_path = "/etc/contextfs/tenant-secret"
audit_log_path = "/var/log/contextfsd-audit.ndjson"

[pdp]
policy_path = "/etc/contextfs/policy.cedar"
default_principal = "Agent::\"pi-sandbox\""

${BROKER_BLOCK}
[[mount]]
name = "work"
mountpoint = "/work"
backend = "remote-fs"
cache_dir = "/var/cache/contextfs/work"
read_only = ${MOUNT_RO}

[mount.remote_fs]
target_uds = "/run/cfs.sock"
CFG_EOF
  fi
  mkdir -p /var/cache/contextfs/work

  # Start contextfsd in background. It probes caps over the
  # bridge once at boot, then FUSE-mounts /work. We poll
  # /proc/mounts up to ~3 s; if /work doesn't appear, log a warn
  # and proceed — the worker still boots without /work.
  /usr/local/bin/contextfsd --config /etc/contextfs/contextfsd.toml \
    >/var/log/contextfsd.log 2>&1 &
  CFSD_PID=$!

  mounted=0
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    if grep -q ' /work fuse' /proc/mounts 2>/dev/null || \
       grep -q ' /work .*fuse' /proc/mounts 2>/dev/null; then
      mounted=1
      break
    fi
    # Bail early if contextfsd died.
    if ! kill -0 "$CFSD_PID" 2>/dev/null; then
      break
    fi
    sleep 0.2
  done
  if [ "$mounted" -ne 1 ]; then
    echo "WARN: contextfs /work mount not visible in /proc/mounts; see /var/log/contextfsd.log"
  fi
fi

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
