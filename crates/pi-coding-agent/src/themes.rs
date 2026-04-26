use pi_tui::{Theme, ThemeRegistry};
use std::path::{Path, PathBuf};

/// Loads themes from the global and project directories. Themes are JSON
/// files matching `Theme`'s schema.
pub fn load_themes(dirs: &[PathBuf]) -> ThemeRegistry {
    let mut reg = ThemeRegistry::new();
    for d in dirs {
        if d.is_dir() {
            if let Ok(rd) = std::fs::read_dir(d) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("json") {
                        if let Some(theme) = read_theme(&p) {
                            reg.install(theme);
                        }
                    }
                }
            }
        }
    }
    reg
}

fn read_theme(path: &Path) -> Option<Theme> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}
