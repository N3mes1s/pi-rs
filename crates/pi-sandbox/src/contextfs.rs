//! Pi-rs ↔ contextfs glue (RFD 0023 §3.5 / Commit G3 prep).
//!
//! Status: **dep-pinning + library-API smoke** only. The full
//! file-sharing path (host-side broker + cfs-fs-server + cfs-mesh
//! tunnel + in-guest contextfsd FUSE mount over vsock) is the
//! next chunk of G3; this module just makes the dep importable
//! and lets us verify the pinned commit is actually checked out.
//!
//! Pinning rationale: contextfs's library API isn't 1.0 yet.
//! Tracking `main` would silently break our build the next time
//! upstream renames a `DaemonConfig` field. The pinned rev below
//! is the source of truth; the workspace `Cargo.toml`'s
//! `contextfsd = { path = "../contextfs/...", ... }` entry uses a
//! path dep today (because the rev isn't on the public remote
//! yet), and the smoke test asserts the local checkout's `git
//! HEAD` matches `EXPECTED_CONTEXTFS_REV`. When the user (a) bumps
//! the pin or (b) accidentally `git pull`s contextfs to a newer
//! commit, the smoke test fails loudly instead of drifting.

/// The contextfs commit pi-rs is pinned to.
///
/// Bump in lockstep with the `Cargo.toml` `contextfsd` dep
/// (currently a path dep; switch to `git + rev` once the
/// pinned commit is on the public remote). The smoke test
/// `tests/contextfs_smoke.rs` asserts the contextfs working
/// tree's `HEAD` equals this string.
pub const EXPECTED_CONTEXTFS_REV: &str =
    "0815009673f37b26bdaea73612099dfd28cc23ef";
