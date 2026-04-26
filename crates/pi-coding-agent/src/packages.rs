//! Package management: `pi install npm:foo`, `pi install git:github.com/u/r`,
//! `pi install https://github.com/u/r@v1`. Packages may declare extensions,
//! skills, prompts, and themes in their `package.json`'s `pi` field, or
//! provide them via convention (top-level `extensions/`, `skills/`,
//! `prompts/`, `themes/` directories).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub pi: Option<PiSection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiSection {
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub themes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub manifest: PackageManifest,
}

/// Discover packages under `package_dir/<name>` directories.
pub fn discover(package_dir: &Path) -> Vec<InstalledPackage> {
    let mut out = Vec::new();
    if !package_dir.is_dir() {
        return out;
    }
    if let Ok(rd) = std::fs::read_dir(package_dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_dir() {
                continue;
            }
            let manifest_path = p.join("package.json");
            let manifest = if manifest_path.is_file() {
                std::fs::read_to_string(&manifest_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<PackageManifest>(&s).ok())
                    .unwrap_or_default()
            } else {
                PackageManifest::default()
            };
            out.push(InstalledPackage {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                path: p,
                manifest,
            });
        }
    }
    out
}

/// Compute resource directories that a package contributes.
pub fn package_dirs(pkg: &InstalledPackage) -> ResourceDirs {
    let mut rd = ResourceDirs::default();
    if let Some(s) = &pkg.manifest.pi {
        for x in &s.extensions {
            rd.extensions.push(pkg.path.join(x));
        }
        for x in &s.skills {
            rd.skills.push(pkg.path.join(x));
        }
        for x in &s.prompts {
            rd.prompts.push(pkg.path.join(x));
        }
        for x in &s.themes {
            rd.themes.push(pkg.path.join(x));
        }
    }
    // Convention-based discovery.
    let conv = [
        ("extensions", &mut rd.extensions),
        ("skills", &mut rd.skills),
        ("prompts", &mut rd.prompts),
        ("themes", &mut rd.themes),
    ];
    for (name, dst) in conv {
        let p = pkg.path.join(name);
        if p.is_dir() && !dst.iter().any(|x| x == &p) {
            dst.push(p);
        }
    }
    rd
}

#[derive(Debug, Default, Clone)]
pub struct ResourceDirs {
    pub extensions: Vec<PathBuf>,
    pub skills: Vec<PathBuf>,
    pub prompts: Vec<PathBuf>,
    pub themes: Vec<PathBuf>,
}

/// Top-level `pi install <spec>` — resolves the spec into a package directory.
/// Supported schemes:
///   - `npm:<name>[@<version>]`
///   - `git:<host>/<owner>/<repo>[@<rev>]`
///   - `https://<host>/<owner>/<repo>[@<rev>]`
pub fn install(spec: &str, package_dir: &Path) -> std::io::Result<InstalledPackage> {
    std::fs::create_dir_all(package_dir)?;
    let (kind, body) = if let Some(rest) = spec.strip_prefix("npm:") {
        ("npm", rest.to_string())
    } else if let Some(rest) = spec.strip_prefix("git:") {
        ("git", rest.to_string())
    } else if let Some(rest) = spec.strip_prefix("https://") {
        ("git", rest.to_string())
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported package spec",
        ));
    };
    let (name_path, version) = match body.split_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (body, None),
    };
    let safe_name = name_path.replace(['/', ':', '\\'], "_");
    let dest = package_dir.join(&safe_name);
    match kind {
        "npm" => {
            let status = std::process::Command::new("npm")
                .args(["install", "--prefix"])
                .arg(&dest)
                .arg(match &version {
                    Some(v) => format!("{}@{}", name_path, v),
                    None => name_path.clone(),
                })
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("npm install failed: {}", s),
                    ))
                }
                Err(e) => return Err(e),
            }
        }
        "git" => {
            let url = if name_path.starts_with("http") {
                name_path.clone()
            } else {
                format!("https://{}", name_path)
            };
            let status = std::process::Command::new("git")
                .args(["clone", "--depth", "1"])
                .arg(&url)
                .arg(&dest)
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("git clone failed: {}", s),
                    ))
                }
                Err(e) => return Err(e),
            }
            if let Some(v) = &version {
                let _ = std::process::Command::new("git")
                    .args(["-C"])
                    .arg(&dest)
                    .args(["checkout", v])
                    .status();
            }
        }
        _ => unreachable!(),
    }
    let manifest_path = dest.join("package.json");
    let manifest = if manifest_path.is_file() {
        std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|s| serde_json::from_str::<PackageManifest>(&s).ok())
            .unwrap_or_default()
    } else {
        PackageManifest::default()
    };
    Ok(InstalledPackage {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        path: dest,
        manifest,
    })
}
