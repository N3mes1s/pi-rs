//! Marker module — no runtime code. See `build.sh` for the rootfs
//! build recipe and `README.md` for the full procedure.
//!
//! The actual rootfs artifact is produced out-of-band by a
//! maintainer running `build.sh` on a Linux host with
//! mkfs.ext4 + zstd available. The artifact is then published
//! as a CI release asset; pi-sandbox::RootfsCache fetches it
//! on first use.

/// Bumped whenever build.sh's output changes shape. Used by
/// pi-sandbox::cache to pin the rootfs URL/sha to a specific
/// version. Embedders never see this — pi-sandbox baked it in
/// at compile time.
pub const ROOTFS_VERSION: &str = "0.1.0";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_string_format() {
        // Sanity: looks like semver.
        let parts: Vec<_> = ROOTFS_VERSION.split('.').collect();
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert!(p.parse::<u32>().is_ok(), "non-numeric: {}", p);
        }
    }
}
