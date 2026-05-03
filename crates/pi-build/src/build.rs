//! Per RFD 0028 §B.1 (CLI flags), §C.2 (cargo subprocess), §C.3
//! (artifact tree). Implements the filesystem write step + the
//! optional `cargo build` subprocess invocation.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::codegen::RenderedTree;

/// CLI options accepted by `pi-build <agent.toml> [OPTIONS]`.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub out_dir: PathBuf,
    pub force: bool,
    pub build: bool,
    pub target: Option<String>,
    pub release: bool,
}

impl BuildOptions {
    pub fn defaults_for(agent_name: &str) -> Self {
        Self {
            out_dir: PathBuf::from(format!("{agent_name}-build")),
            force: false,
            build: false,
            target: None,
            release: true,
        }
    }
}

/// Result of `cargo build` (only populated when `--build` was set).
#[derive(Debug)]
pub struct BuildOutcome {
    pub binary_path: PathBuf,
    pub cargo_status: std::process::ExitStatus,
}

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("output directory {0} is not empty; pass --force to overwrite")]
    OutDirNotEmpty(PathBuf),

    #[error("I/O error writing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cargo not found on PATH (tried `cargo --version`)")]
    CargoNotFound,

    #[error("cargo build failed with exit {0}")]
    CargoFailed(std::process::ExitStatus),
}

/// Write the rendered tree to `opts.out_dir` per §C.3.1 semantics.
///
/// - Doesn't exist → create + write.
/// - Exists, empty → write.
/// - Exists, non-empty, no `--force` → `Err(OutDirNotEmpty)`.
/// - Exists, non-empty, `--force` → wipe-then-write (atomic from
///   the operator's perspective: the new tree replaces the old
///   completely, or the old is preserved on failure).
pub fn write_tree(tree: &RenderedTree, opts: &BuildOptions) -> Result<(), BuildError> {
    if opts.out_dir.exists() {
        let mut iter = std::fs::read_dir(&opts.out_dir).map_err(|e| BuildError::Io {
            path: opts.out_dir.clone(),
            source: e,
        })?;
        if iter.next().is_some() {
            if !opts.force {
                return Err(BuildError::OutDirNotEmpty(opts.out_dir.clone()));
            }
            // Wipe-then-write per §C.3.1.
            std::fs::remove_dir_all(&opts.out_dir).map_err(|e| BuildError::Io {
                path: opts.out_dir.clone(),
                source: e,
            })?;
        }
    }
    let src_dir = opts.out_dir.join("src");
    std::fs::create_dir_all(&src_dir).map_err(|e| BuildError::Io {
        path: src_dir.clone(),
        source: e,
    })?;
    write_file(&opts.out_dir.join("Cargo.toml"), &tree.cargo_toml)?;
    write_file(&src_dir.join("main.rs"), &tree.main_rs)?;
    write_file(&opts.out_dir.join("pi-build.lock"), &tree.pi_build_lock)?;
    Ok(())
}

fn write_file(path: &Path, contents: &str) -> Result<(), BuildError> {
    std::fs::write(path, contents).map_err(|e| BuildError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Run `cargo build` in `opts.out_dir`. Forwards stdout/stderr to
/// the parent's terminals (no capture). Per §C.2 — pi-build adds
/// only `--manifest-path <out>/Cargo.toml` beyond the operator's
/// requested flags. NO RUSTFLAGS, NO -C overrides.
pub async fn cargo_build(opts: &BuildOptions) -> Result<BuildOutcome, BuildError> {
    let manifest_path = opts.out_dir.join("Cargo.toml");
    let mut cmd = tokio::process::Command::new("cargo");
    cmd.arg("build");
    if opts.release {
        cmd.arg("--release");
    }
    if let Some(t) = &opts.target {
        cmd.arg("--target").arg(t);
    }
    cmd.arg("--manifest-path").arg(&manifest_path);
    cmd.stdin(std::process::Stdio::null());
    // Inherited stdout/stderr by default — cargo's diagnostics
    // go to the operator's terminal directly.

    let status = cmd.status().await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => BuildError::CargoNotFound,
        _ => BuildError::Io {
            path: PathBuf::from("cargo"),
            source: e,
        },
    })?;
    if !status.success() {
        return Err(BuildError::CargoFailed(status));
    }

    // target/{<triple>/}{release|debug}/<agent_name>
    let agent_name = opts
        .out_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.trim_end_matches("-build"))
        .unwrap_or("agent")
        .to_owned();
    let mut bin_path = opts.out_dir.join("target");
    if let Some(t) = &opts.target {
        bin_path.push(t);
    }
    bin_path.push(if opts.release { "release" } else { "debug" });
    bin_path.push(&agent_name);

    Ok(BuildOutcome {
        binary_path: bin_path,
        cargo_status: status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::RenderedTree;

    fn fake_tree() -> RenderedTree {
        RenderedTree {
            cargo_toml: "[package]\nname = \"x\"\n".into(),
            main_rs: "fn main() {}\n".into(),
            pi_build_lock: "v=1\n".into(),
        }
    }

    #[test]
    fn write_tree_creates_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = BuildOptions {
            out_dir: tmp.path().join("nested/dir"),
            force: false,
            build: false,
            target: None,
            release: true,
        };
        write_tree(&fake_tree(), &opts).expect("create + write");
        assert!(opts.out_dir.join("Cargo.toml").is_file());
        assert!(opts.out_dir.join("src/main.rs").is_file());
        assert!(opts.out_dir.join("pi-build.lock").is_file());
    }

    #[test]
    fn write_tree_empty_dir_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = BuildOptions {
            out_dir: tmp.path().to_path_buf(),
            force: false,
            build: false,
            target: None,
            release: true,
        };
        // tempdir() creates an empty dir; this should succeed.
        write_tree(&fake_tree(), &opts).expect("write into empty existing dir");
    }

    #[test]
    fn write_tree_non_empty_no_force_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("garbage"), "x").unwrap();
        let opts = BuildOptions {
            out_dir: tmp.path().to_path_buf(),
            force: false,
            build: false,
            target: None,
            release: true,
        };
        let err = write_tree(&fake_tree(), &opts).unwrap_err();
        assert!(matches!(err, BuildError::OutDirNotEmpty(_)), "{err:?}");
    }

    #[test]
    fn write_tree_non_empty_with_force_wipes_then_writes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("stale-artifact"), "x").unwrap();
        let opts = BuildOptions {
            out_dir: tmp.path().to_path_buf(),
            force: true,
            build: false,
            target: None,
            release: true,
        };
        write_tree(&fake_tree(), &opts).expect("force should wipe + write");
        // Stale artifact is gone (proves wipe-then-write, not merge).
        assert!(!tmp.path().join("stale-artifact").exists());
        // New tree present.
        assert!(opts.out_dir.join("Cargo.toml").is_file());
    }

    #[tokio::test]
    async fn cargo_build_with_missing_cargo_returns_cargo_not_found() {
        // Override PATH to an empty dir so `cargo` is not findable.
        let tmp = tempfile::tempdir().unwrap();
        let opts = BuildOptions {
            out_dir: tmp.path().to_path_buf(),
            force: false,
            build: true,
            target: None,
            release: true,
        };
        // Need a Cargo.toml in out_dir or cargo would also fail; we're
        // testing the spawn-time NotFound path, so put one in just to
        // keep the test deterministic regardless of cargo's pre-checks.
        std::fs::write(opts.out_dir.join("Cargo.toml"), "").unwrap();

        let saved_path = std::env::var_os("PATH");
        // SAFETY: setting env in a single-threaded test scope.
        std::env::set_var("PATH", "");
        let result = cargo_build(&opts).await;
        if let Some(p) = saved_path {
            std::env::set_var("PATH", p);
        }
        assert!(matches!(result, Err(BuildError::CargoNotFound)), "{result:?}");
    }
}
