# pi-sandbox-rootfs

Maintainer build recipe for the pi-rs microVM sandbox rootfs artifact.

## What this crate is

This crate contains **no runtime Rust code**. Its sole purpose is:

1. **`build.sh`** — A shell script that assembles the rootfs image from scratch
   on a Linux host.
2. **`src/lib.rs`** — A Rust marker with a single `ROOTFS_VERSION` constant used
   by `pi-sandbox::cache` to pin the expected artifact version at compile time.
3. **This README** — Full procedure for maintainers who need to publish a new
   rootfs version.

## What the artifact is

The rootfs is an **ext4 image compressed with zstd** containing:

- **Alpine Linux 3.19** minirootfs (~6 MB) as the base OS.
- **`pi-sandbox-worker`** (statically linked musl binary, ~6–8 MB) placed at
  `/usr/local/bin/pi-sandbox-worker`. This is the process the init script
  executes inside the VM.
- **`/init`** — A tiny POSIX shell script that:
  1. Mounts `/proc`, `/sys`, and the virtio-fs host share at `/work`.
  2. Reads `/proc/cmdline` for `pi.proto_version=N`; if the value doesn't match
     the compiled-in expectation, it prints a fatal diagnostic and halts.
  3. Execs `pi-sandbox-worker --vsock-port=5001 --work-dir=/work`.
- **`/etc/pi-sandbox-version`** and **`/etc/pi-sandbox-proto-version`** — plain
  text stamps the `pi-sandbox::cache` module can inspect at boot.

The finished artifact is named
`pi-sandbox-rootfs-vMAJOR.MINOR.PATCH.img.zst` and its SHA256 is published
alongside the artifact.

## Prerequisites

| Tool | Min version | Install hint |
|------|-------------|--------------|
| `cargo` | (workspace version) | via rustup |
| `mkfs.ext4` | e2fsprogs ≥ 1.42.4 | `apt install e2fsprogs` / `pacman -S e2fsprogs` |
| `zstd` | any recent | `apt install zstd` / `pacman -S zstd` |
| `curl` | any | standard |
| `tar` | any | standard |
| Rust target `x86_64-unknown-linux-musl` | — | `rustup target add x86_64-unknown-linux-musl` |

> **Note:** `mkfs.ext4 -d <directory>` (unprivileged rootfs population) was
> added in e2fsprogs 1.42.4. On older systems you'll need `fakeroot` or must
> run as root. Ubuntu 20.04+ ships a new enough version; Debian 10+ does too.

## How to run `build.sh`

```bash
# From the repo root:
bash crates/pi-sandbox-rootfs/build.sh [VERSION]
```

`VERSION` defaults to `0.1.0`. The script:

1. Cross-compiles `pi-sandbox-worker` for `x86_64-unknown-linux-musl` (release).
2. Downloads the Alpine minirootfs tarball (cached in `target/pi-sandbox-rootfs/cache/`).
3. Stages the rootfs tree in a temp directory.
4. Produces `target/pi-sandbox-rootfs/pi-sandbox-rootfs-vVERSION.img.zst`.
5. Prints the SHA256 + size + the `ROOTFS_MANIFEST` snippet to paste into
   `crates/pi-sandbox/src/cache.rs`.

## Output location

```
target/pi-sandbox-rootfs/
├── cache/
│   └── alpine-minirootfs-3.19.0-x86_64.tar.gz   # cached download
├── pi-sandbox-rootfs-v0.1.0.img                  # intermediate (large; auto-deleted)
└── pi-sandbox-rootfs-v0.1.0.img.zst              # the artifact to publish
```

`target/` is gitignored; the artifact never lands in version control.

## Publishing as a release asset

1. Run `build.sh` on a clean Linux host (ideally in CI to avoid "works on my
   machine" drift).
2. Create a GitHub release tagged `sandbox-rootfs-v<VERSION>`.
3. Upload `pi-sandbox-rootfs-v<VERSION>.img.zst` as a release asset.
4. Copy the `ROOTFS_MANIFEST` snippet printed by `build.sh` into
   `crates/pi-sandbox/src/cache.rs` — the four constants
   `ROOTFS_VERSION`, `ROOTFS_URL`, `ROOTFS_SHA256`, and `ROOTFS_SIZE_BYTES`.
5. Bump `ROOTFS_VERSION` in `crates/pi-sandbox-rootfs/src/lib.rs` to match.
6. Open a PR. CI will build the workspace (the rootfs artifact itself is
   downloaded on first use, not at `cargo build` time).

> The CLI flag `--sandbox-provider=microvm` does **not** ship until all three
> OS launchers (Firecracker / vfkit / cloud-hypervisor) are merged. Until then
> the cache download path is exercised only by tests.

## What `pi-sandbox::cache` does with this

On first use of `--sandbox-provider=microvm`, `pi-sandbox::RootfsCache::ensure()`
checks `~/.cache/pi/sandbox/rootfs/<version>/rootfs.img.zst`:

- **Present + SHA256 matches** → use it directly.
- **Present + SHA256 mismatch** → delete + re-download (corruption guard).
- **Absent** → download from `ROOTFS_URL` with HTTP Range resume, then verify
  SHA256 before returning the path.
- **`PI_SANDBOX_ROOTFS=/path` env override** → skip all of the above and use
  that path (useful for offline development and custom builds).
- **`PI_SANDBOX_OFFLINE=1`** → refuse to download; fail with a clear error if
  the cached artifact is missing.

See `crates/pi-sandbox/src/cache.rs` for the full implementation and
`crates/pi-sandbox/tests/cache_smoke.rs` for the test suite.
