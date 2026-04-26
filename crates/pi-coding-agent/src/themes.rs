use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pi_tui::{Theme, ThemeRegistry};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Loads themes from the global and project directories. Themes are JSON
/// files matching `Theme`'s schema.
pub fn load_themes(dirs: &[PathBuf]) -> ThemeRegistry {
    let mut reg = ThemeRegistry::new();
    for d in dirs {
        load_into(d, &mut reg);
    }
    reg
}

pub fn load_into(d: &Path, reg: &mut ThemeRegistry) {
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

pub fn read_theme(path: &Path) -> Option<Theme> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// Wrap a `ThemeRegistry` in a hot-reloadable handle. Returns the handle
/// and the watcher (kept alive for the lifetime of the caller — drop to
/// stop watching).
pub struct HotThemes {
    pub registry: Arc<Mutex<ThemeRegistry>>,
    _watcher: Option<RecommendedWatcher>,
}

impl HotThemes {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        let registry = Arc::new(Mutex::new(load_themes(&dirs)));
        let reg = registry.clone();
        let dirs_for_watch = dirs.clone();
        let watcher = match RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(ev) = res {
                    if matches!(
                        ev.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    ) {
                        if let Ok(mut g) = reg.lock() {
                            *g = load_themes(&dirs_for_watch);
                        }
                    }
                }
            },
            notify::Config::default(),
        ) {
            Ok(mut w) => {
                for d in &dirs {
                    if d.is_dir() {
                        let _ = w.watch(d, RecursiveMode::NonRecursive);
                    }
                }
                Some(w)
            }
            Err(_) => None,
        };
        Self {
            registry,
            _watcher: watcher,
        }
    }

    pub fn snapshot(&self) -> ThemeRegistry {
        self.registry.lock().map(|g| g.clone()).unwrap_or_default()
    }
}
