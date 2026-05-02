//! Rootfs cache — fetch the alpine+worker image artifact on
//! first sandbox use, verify sha256, support resume-on-partial.
//!
//! The artifact is published per RFD 0023 Commit B; this module
//! is what consumers (FirecrackerLauncher, VfkitLauncher,
//! CloudHypervisorLauncher) call before booting a VM.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(String),
    #[error("sha256 mismatch: expected {expected}, got {found}")]
    Sha256Mismatch { expected: String, found: String },
    #[error("size mismatch: expected {expected}, got {found}")]
    SizeMismatch { expected: u64, found: u64 },
    #[error("offline: PI_SANDBOX_OFFLINE=1 set and rootfs not in cache")]
    OfflineMissingRootfs,
    #[error("interrupted")]
    Interrupted,
}

/// Static manifest baked at build time. The build.sh recipe
/// emits the values; the maintainer pastes them here on each
/// rootfs version bump.
pub const ROOTFS_VERSION: &str = "0.1.0";
pub const ROOTFS_URL: &str =
    "https://github.com/pi-rs/releases/download/sandbox-rootfs-v0.1.0/pi-sandbox-rootfs-v0.1.0.img.zst";
pub const ROOTFS_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
pub const ROOTFS_SIZE_BYTES: u64 = 0;

/// Where cached artifacts live by default. `~/.cache/pi/sandbox/rootfs/<version>/rootfs.img.zst`
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pi/sandbox/rootfs")
}

pub struct RootfsCache {
    cache_dir: PathBuf,
}

impl RootfsCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    pub fn with_default_dir() -> Self {
        Self::new(default_cache_dir())
    }

    /// Where the cached artifact for the configured version lives.
    pub fn artifact_path(&self, version: &str) -> PathBuf {
        self.cache_dir.join(version).join("rootfs.img.zst")
    }

    /// Ensure the rootfs is present, sha-verified, and return its path.
    ///
    /// Download strategy:
    /// - Full file present + sha matches → return immediately (no HTTP).
    /// - Full file present + sha mismatch → delete + re-download from scratch.
    /// - Partial file present (size < expected_size) → resume via HTTP Range.
    /// - No file → download from scratch.
    ///
    /// After any download, sha256 is verified before returning. On mismatch
    /// the corrupt file is deleted and an error is returned (caller may retry).
    ///
    /// Env overrides:
    /// - `PI_SANDBOX_ROOTFS=/path` → return that path directly (no HTTP, no verify).
    /// - `PI_SANDBOX_OFFLINE=1` → refuse to download; error if not cached.
    pub async fn ensure(
        &self,
        version: &str,
        url: &str,
        expected_sha256: &str,
        expected_size: u64,
    ) -> Result<PathBuf, CacheError> {
        // 1. Env path override — skip all cache logic.
        if let Ok(path) = std::env::var("PI_SANDBOX_ROOTFS") {
            return Ok(PathBuf::from(path));
        }

        let path = self.artifact_path(version);
        let existing_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        // 2. Full file on disk — verify sha.
        if path.is_file() && existing_size >= expected_size && expected_size > 0 {
            if verify_sha256(&path, expected_sha256).await? {
                return Ok(path);
            }
            // Complete but corrupt → delete + re-download from scratch.
            std::fs::remove_file(&path)?;
        }
        // else: partial file (existing_size < expected_size) → fall through to resume.

        // 3. Offline guard — refuse to download.
        if std::env::var("PI_SANDBOX_OFFLINE").as_deref() == Ok("1") {
            return Err(CacheError::OfflineMissingRootfs);
        }

        // 4. Download (fresh start or resume).
        std::fs::create_dir_all(path.parent().unwrap())?;
        download_with_resume(url, &path, expected_size).await?;

        // 5. Verify sha of the completed download.
        if !verify_sha256(&path, expected_sha256).await? {
            let actual = compute_sha256(&path).await?;
            // Clean up so the next call re-downloads rather than loop-verifying.
            let _ = std::fs::remove_file(&path);
            return Err(CacheError::Sha256Mismatch {
                expected: expected_sha256.into(),
                found: actual,
            });
        }
        Ok(path)
    }
}

async fn download_with_resume(
    url: &str,
    path: &Path,
    expected_size: u64,
) -> Result<(), CacheError> {
    use tokio::io::AsyncWriteExt;
    let existing = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if existing >= expected_size && expected_size > 0 {
        return Ok(()); // already done (shouldn't reach here normally)
    }
    let client = reqwest::Client::new();
    let mut req = client.get(url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let resp = req.send().await.map_err(|e| CacheError::Http(e.to_string()))?;
    if !(resp.status().is_success() || resp.status() == reqwest::StatusCode::PARTIAL_CONTENT) {
        return Err(CacheError::Http(format!("status {}", resp.status())));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CacheError::Http(e.to_string()))?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

async fn compute_sha256(path: &Path) -> Result<String, CacheError> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn verify_sha256(path: &Path, expected: &str) -> Result<bool, CacheError> {
    Ok(compute_sha256(path).await? == expected)
}
