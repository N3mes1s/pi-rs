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
