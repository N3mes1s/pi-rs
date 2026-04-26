use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A skill loaded from the filesystem. Conforms to the
/// [Agent Skills](https://agentskills.io) layout: a directory with
/// `SKILL.md` (or a single `name.md` file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    inner: BTreeMap<String, Skill>,
}

impl SkillRegistry {
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
                if p.is_dir() {
                    let sk_md = p.join("SKILL.md");
                    if sk_md.is_file() {
                        if let Some(skill) = read_skill(&sk_md) {
                            self.inner.insert(skill.name.clone(), skill);
                        }
                    }
                } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Some(skill) = read_skill(&p) {
                        self.inner.insert(skill.name.clone(), skill);
                    }
                }
            }
        }
    }

    pub fn add(&mut self, skill: Skill) {
        self.inner.insert(skill.name.clone(), skill);
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.inner.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.inner.keys().cloned().collect()
    }
}

fn read_skill(path: &Path) -> Option<Skill> {
    let body = std::fs::read_to_string(path).ok()?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })?;
    let description = body
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some(Skill {
        name,
        description,
        body,
        path: path.to_path_buf(),
    })
}
