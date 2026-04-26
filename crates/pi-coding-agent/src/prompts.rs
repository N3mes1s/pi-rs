use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A `/<name>` prompt template loaded from `~/.pi/agent/prompts/<name>.md`.
/// Supports `{{var}}` interpolation.
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub body: String,
    pub path: PathBuf,
}

impl PromptTemplate {
    pub fn render(&self, vars: &std::collections::HashMap<String, String>) -> String {
        let mut out = self.body.clone();
        for (k, v) in vars {
            let needle = format!("{{{{{}}}}}", k);
            out = out.replace(&needle, v);
        }
        out
    }
}

#[derive(Debug, Default, Clone)]
pub struct PromptRegistry {
    inner: BTreeMap<String, PromptTemplate>,
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_all(&mut self, dirs: &[PathBuf]) {
        for d in dirs {
            self.load_dir(d);
        }
    }

    pub fn load_dir(&mut self, dir: &Path) {
        if !dir.is_dir() {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Ok(body) = std::fs::read_to_string(&p) {
                        let name = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        if !name.is_empty() {
                            self.inner.insert(
                                name.clone(),
                                PromptTemplate {
                                    name,
                                    body,
                                    path: p,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    pub fn add(&mut self, t: PromptTemplate) {
        self.inner.insert(t.name.clone(), t);
    }

    pub fn get(&self, name: &str) -> Option<&PromptTemplate> {
        self.inner.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }
}
