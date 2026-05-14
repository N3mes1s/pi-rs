//! Canonical contextfsd embedder-mode TOML generator.
//!
//! Used by:
//! - The Sprites remote-sandbox provider (`remote::sprites`) to write
//!   `/etc/contextfs/contextfsd.toml` inside the sprite (Phase C of
//!   RFD 0026 v2).
//! - The local-mount regression test
//!   (`tests/contextfs_local_rw_mount.rs`).
//! - Future microvm callers that want a programmatic generator instead
//!   of the inline heredoc in `crates/pi-sandbox-rootfs/build.sh`.
//!
//! Single source of truth so the three call sites can't drift.
//!
//! Embedder profile (per project memory + RFD 0023 §3.5):
//!   * `caller_uid_passthrough = true` — every FUSE op is replied to
//!     with the caller's uid/gid, so non-mounting UIDs (a bash drop-priv
//!     under the agent worker, the unprivileged sprite user) still see
//!     owner perms on `/work`. Single-uid sandbox row of contextfs's
//!     threat model.
//!   * `fuse_acl` — `Auto` for local non-root daemons (becomes Owner ACL),
//!     `All` for production sprite/microvm where the daemon is root or
//!     `user_allow_other` is set in `/etc/fuse.conf`.
//!   * `auto_unmount = true` — kernel detaches the mountpoint on hard
//!     daemon death; depends on `fusermount3` (the `fuse3` package).
//!   * `read_only = false` — default; flip to `true` to lock /work down.

use std::path::{Path, PathBuf};

/// Which `fuse_acl` value the daemon TOML should request.
///
/// Maps 1:1 to contextfsd's `MountConfig.fuse_acl` field. See the
/// `[[mount]] fuse_acl` table in `contextfs/README.md` for the
/// dispatch rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuseAcl {
    /// `"auto"`: daemon detects effective UID at runtime — root
    /// daemons get `SessionACL::All`, non-root daemons get
    /// `SessionACL::Owner`. The local-test default.
    Auto,
    /// `"all"`: force `SessionACL::All`. Required when the daemon is
    /// non-root and additional UIDs need access (e.g. an unprivileged
    /// tool worker reading `/work`). Requires `user_allow_other` in
    /// `/etc/fuse.conf` on non-root daemons. The production-embedder
    /// default.
    All,
    /// `"owner"`: force `SessionACL::Owner` even on root daemons.
    /// Hermetic CI shape; rarely the right choice for an agent
    /// sandbox.
    Owner,
}

impl FuseAcl {
    fn as_toml_str(self) -> &'static str {
        match self {
            FuseAcl::Auto => "auto",
            FuseAcl::All => "all",
            FuseAcl::Owner => "owner",
        }
    }
}

/// Builder for an embedder-profile `contextfsd.toml`. Every field is
/// required (no defaults beyond `read_only = false`) so a caller can't
/// forget a path and silently get a broken daemon.
#[derive(Clone, Debug)]
pub struct EmbedderTomlSpec {
    /// Path to the tenant-secret file (32 hex bytes, mode 0600).
    pub tenant_secret_path: PathBuf,
    /// Path the daemon will write its audit log to (ndjson).
    pub audit_log_path: PathBuf,
    /// Path to the Cedar policy text (see
    /// `microvm::broker_proxy::resolved_cedar_policy_text`).
    pub cedar_policy_path: PathBuf,
    /// Cedar principal entity-id, e.g. `Agent::"pi-sprite"`.
    /// Must match the broker's accepted principal.
    pub principal: String,
    /// UDS path the daemon dials for `verify_write` (the host-side
    /// `contextfs-broker run --socket …`).
    pub broker_socket_path: PathBuf,
    /// `[[mount]] name`. Typically `"work"`.
    pub mount_name: String,
    /// Where in the sandbox/host FS to mount `/work`.
    pub mountpoint: PathBuf,
    /// Directory the daemon uses for the per-mount cache. Must be
    /// writable by the daemon UID. Budget defaults to 1 GiB on the
    /// daemon side; pinning here would be a follow-up knob.
    pub cache_dir: PathBuf,
    /// Where the `RemoteFsBackend` dials cfs-fs-server. In the Sprites
    /// path this is the local UDS exposed by `cfs-mesh receive-uds`
    /// (the sandbox-side listener); in microvm it's `/run/cfs.sock`
    /// (the vsock-bridge endpoint).
    pub remote_fs_target_uds: PathBuf,
    pub fuse_acl: FuseAcl,
    pub read_only: bool,
}

impl EmbedderTomlSpec {
    /// Render the canonical embedder TOML. Output is deterministic and
    /// suitable for byte-for-byte assertions in tests.
    pub fn render(&self) -> String {
        // Inline `toml::to_string` would lose the field ordering and
        // forfeit the in-body documenting comments we want operators
        // to see. Hand-formatted is fine — the schema is small and the
        // single source of truth here means there's nothing to drift.
        format!(
            r#"tenant_secret_path = {tenant_secret:?}
audit_log_path = {audit_log:?}

[pdp]
policy_path = {policy:?}
default_principal = '{principal}'

[broker]
socket_path = {broker:?}

[[mount]]
name = {mount_name:?}
mountpoint = {mountpoint:?}
backend = "remote-fs"
cache_dir = {cache:?}
caller_uid_passthrough = true
fuse_acl = {fuse_acl:?}
auto_unmount = true
read_only = {read_only}

[mount.remote_fs]
target_uds = {remote_fs_uds:?}
"#,
            tenant_secret = self.tenant_secret_path.display().to_string(),
            audit_log = self.audit_log_path.display().to_string(),
            policy = self.cedar_policy_path.display().to_string(),
            principal = self.principal,
            broker = self.broker_socket_path.display().to_string(),
            mount_name = self.mount_name,
            mountpoint = self.mountpoint.display().to_string(),
            cache = self.cache_dir.display().to_string(),
            fuse_acl = self.fuse_acl.as_toml_str(),
            read_only = self.read_only,
            remote_fs_uds = self.remote_fs_target_uds.display().to_string(),
        )
    }

    /// Convenience constructor for the Sprites-embedder shape: every
    /// path defaults to the standard sprite layout under
    /// `/etc/contextfs/` and `/run/contextfs/`, with `fuse_acl = All`
    /// and `read_only = false`. The caller still has to materialise
    /// the supporting files (tenant secret, policy) and configure the
    /// receive-uds endpoint.
    pub fn sprite_default(remote_fs_target_uds: impl Into<PathBuf>) -> Self {
        Self {
            tenant_secret_path: PathBuf::from("/etc/contextfs/tenant-secret"),
            audit_log_path: PathBuf::from("/var/log/contextfsd-audit.ndjson"),
            cedar_policy_path: PathBuf::from("/etc/contextfs/policy.cedar"),
            principal: r#"Agent::"pi-sprite""#.into(),
            broker_socket_path: PathBuf::from("/run/contextfs/broker.sock"),
            mount_name: "work".into(),
            mountpoint: PathBuf::from("/work"),
            cache_dir: PathBuf::from("/var/cache/contextfs/work"),
            remote_fs_target_uds: remote_fs_target_uds.into(),
            fuse_acl: FuseAcl::All,
            read_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ws_path(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    fn local_spec(mountpoint: &Path, fs_uds: &Path) -> EmbedderTomlSpec {
        EmbedderTomlSpec {
            tenant_secret_path: ws_path("/tmp/x/tenant.secret"),
            audit_log_path: ws_path("/tmp/x/audit.ndjson"),
            cedar_policy_path: ws_path("/tmp/x/policy.cedar"),
            principal: r#"Agent::"pi-spike""#.into(),
            broker_socket_path: ws_path("/tmp/x/broker.sock"),
            mount_name: "work".into(),
            mountpoint: mountpoint.into(),
            cache_dir: ws_path("/tmp/x/cache"),
            remote_fs_target_uds: fs_uds.into(),
            fuse_acl: FuseAcl::Auto,
            read_only: false,
        }
    }

    #[test]
    fn render_contains_required_fields() {
        let mnt = ws_path("/tmp/work");
        let uds = ws_path("/tmp/x/cfs-fs.sock");
        let t = local_spec(&mnt, &uds).render();

        for needle in [
            "tenant_secret_path",
            "audit_log_path",
            "[pdp]",
            "policy_path",
            "default_principal = 'Agent::\"pi-spike\"'",
            "[broker]",
            "socket_path",
            "[[mount]]",
            "name = \"work\"",
            "mountpoint",
            "backend = \"remote-fs\"",
            "caller_uid_passthrough = true",
            "fuse_acl = \"auto\"",
            "auto_unmount = true",
            "read_only = false",
            "[mount.remote_fs]",
            "target_uds",
        ] {
            assert!(t.contains(needle), "rendered TOML missing {needle:?}; got:\n{t}");
        }
    }

    #[test]
    fn fuse_acl_variants_serialise() {
        let mnt = ws_path("/tmp/work");
        let uds = ws_path("/tmp/x/cfs-fs.sock");
        let mut spec = local_spec(&mnt, &uds);
        for (variant, expect) in [
            (FuseAcl::Auto, "fuse_acl = \"auto\""),
            (FuseAcl::All, "fuse_acl = \"all\""),
            (FuseAcl::Owner, "fuse_acl = \"owner\""),
        ] {
            spec.fuse_acl = variant;
            let rendered = spec.render();
            assert!(rendered.contains(expect), "missing {expect:?}");
        }
    }

    #[test]
    fn read_only_flips_correctly() {
        let mnt = ws_path("/tmp/work");
        let uds = ws_path("/tmp/x/cfs-fs.sock");
        let mut spec = local_spec(&mnt, &uds);
        assert!(spec.render().contains("read_only = false"));
        spec.read_only = true;
        assert!(spec.render().contains("read_only = true"));
    }

    #[test]
    fn round_trips_through_toml_parser() {
        // Sanity: the rendered string parses back as TOML, validating
        // we didn't break the wire shape (no unquoted paths with
        // unusual chars, no missing tables).
        let mnt = ws_path("/tmp/work-mount");
        let uds = ws_path("/tmp/x/cfs-fs.sock");
        let t = local_spec(&mnt, &uds).render();
        let parsed: toml::Value = toml::from_str(&t).expect("rendered TOML must parse");
        let mount = parsed
            .get("mount")
            .and_then(|m| m.as_array())
            .and_then(|a| a.first())
            .expect("mount array");
        assert_eq!(
            mount.get("caller_uid_passthrough").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            mount.get("backend").and_then(|v| v.as_str()),
            Some("remote-fs")
        );
        let remote_fs = mount
            .get("remote_fs")
            .and_then(|v| v.as_table())
            .expect("remote_fs sub-table");
        assert_eq!(
            remote_fs.get("target_uds").and_then(|v| v.as_str()),
            Some(uds.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn sprite_default_uses_production_paths() {
        let s = EmbedderTomlSpec::sprite_default("/run/contextfs/cfs-fs.sock");
        assert_eq!(
            s.tenant_secret_path,
            PathBuf::from("/etc/contextfs/tenant-secret")
        );
        assert_eq!(s.mountpoint, PathBuf::from("/work"));
        assert_eq!(s.fuse_acl, FuseAcl::All);
        assert!(!s.read_only);
        let rendered = s.render();
        assert!(rendered.contains(r#"default_principal = 'Agent::"pi-sprite"'"#));
        assert!(rendered.contains("fuse_acl = \"all\""));
    }
}
