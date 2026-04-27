//! Top-level subcommands: install / list / config / update.
//! Used when the `--install`, `--list`, `--config`, or `--update` flags are
//! present (mirroring `pi install`, `pi list`, etc.).

use crate::context::{agent_dir, package_dir};
use crate::packages;

pub fn run_install(spec: &str) -> anyhow::Result<()> {
    let dest = package_dir();
    let pkg = packages::install(spec, &dest)?;
    println!(
        "installed {} ({}) -> {}",
        pkg.name.is_empty().then(|| spec.to_string()).unwrap_or(pkg.name.clone()),
        pkg.version,
        pkg.path.display()
    );
    Ok(())
}

pub fn run_list() -> anyhow::Result<()> {
    let pkgs = packages::discover(&package_dir());
    if pkgs.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    for pkg in pkgs {
        println!("- {} {}  ({})", pkg.name, pkg.version, pkg.path.display());
    }
    Ok(())
}

pub fn run_config() -> anyhow::Result<()> {
    let dir = agent_dir();
    println!("agent dir: {}", dir.display());
    println!("settings:  {}", dir.join("settings.json").display());
    println!("auth:      {}", dir.join("auth.json").display());
    println!("sessions:  {}", dir.join("sessions").display());
    println!("packages:  {}", package_dir().display());
    Ok(())
}

pub fn run_update() -> anyhow::Result<()> {
    let pkgs = packages::discover(&package_dir());
    for pkg in pkgs {
        println!("updating {} …", pkg.name);
        let _ = std::process::Command::new("git")
            .args(["-C"])
            .arg(&pkg.path)
            .arg("pull")
            .status();
    }
    Ok(())
}

/// `pi --refresh-models` — query every provider with credentials for its
/// live model catalogue, merge into `<agent_dir>/discovered-models.json`,
/// and report per-provider success/failure.
///
/// This needs an async runtime, so it can't sit in the synchronous fast-path
/// in `bin/pi.rs`; the binary spins one up on demand.
pub async fn run_refresh_models() -> anyhow::Result<()> {
    use crate::context::{agent_dir, auth_path};
    use pi_ai::{discovered_cache_path, refresh_and_save, AuthStorage, ModelRegistry};

    // Load creds: file first, env second (env wins).
    let auth = AuthStorage::open(auth_path()).unwrap_or_else(|_| AuthStorage::in_memory());
    let env = AuthStorage::from_env();
    for (p, _) in AuthStorage::ENV_KEYS {
        if let Some(m) = env.get(p) {
            auth.set(p, m);
        }
    }
    let registry = ModelRegistry::new(auth.clone());

    let cache_path = discovered_cache_path(&agent_dir());
    let (cache, results) = refresh_and_save(&registry, &auth, &cache_path).await?;

    println!("discovered-models cache: {}", cache_path.display());
    let mut total = 0usize;
    for r in &results {
        match &r.result {
            Ok(models) => {
                total += models.len();
                println!("  ✓ {} → {} models", r.provider, models.len());
            }
            Err(e) => println!("  ✗ {} → {}", r.provider, e),
        }
    }
    println!("total: {} models across {} providers", total, cache.providers.len());
    Ok(())
}
