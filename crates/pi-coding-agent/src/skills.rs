use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ── built-in skills, embedded at compile time ────────────────────────────────

/// Native skills that ship with pi-rs. Each is a `(folder_name, SKILL.md
/// contents)` pair; on first use they are materialised to a stable temp
/// directory so the existing on-disk skill loader works unchanged.
const BUILTIN_SKILLS: &[(&str, &str)] = &[(
    "autoresearch-create",
    include_str!("../skills/autoresearch-create/SKILL.md"),
)];

/// Materialise the built-in skills under `$PI_BUILTIN_SKILLS_DIR` (env
/// override) or `<TMP>/pi-rs-builtin-skills/`. Returns the directory path
/// once written; idempotent — re-running just overwrites the SKILL.md.
pub fn ensure_builtin_skills_dir() -> std::io::Result<PathBuf> {
    let base = match std::env::var_os("PI_BUILTIN_SKILLS_DIR") {
        Some(p) => PathBuf::from(p),
        None => std::env::temp_dir().join("pi-rs-builtin-skills"),
    };
    for (name, body) in BUILTIN_SKILLS {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("SKILL.md");
        std::fs::write(path, body)?;
    }
    Ok(base)
}

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

/// Format the available-skills block that gets prepended to the system
/// prompt. Faithful port of upstream `formatSkillsForPrompt` (see
/// `pi-mono/packages/coding-agent/src/core/skills.ts`):
/// the agent receives a list of `<skill name=… description=… location=…>`
/// entries, instructed to read the SKILL.md via the `read` tool when the
/// task matches the description.
pub fn format_skills_for_prompt(skills: &[&Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    s.push_str("\n\nThe following skills provide specialized instructions for specific tasks.\n");
    s.push_str("Use the read tool to load a skill's file when the task matches its description.\n");
    s.push_str(
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n",
    );
    s.push_str("\n<available_skills>\n");
    for skill in skills {
        s.push_str("  <skill>\n");
        s.push_str(&format!("    <name>{}</name>\n", xml_escape(&skill.name)));
        s.push_str(&format!(
            "    <description>{}</description>\n",
            xml_escape(&skill.description)
        ));
        s.push_str(&format!(
            "    <location>{}</location>\n",
            xml_escape(&skill.path.display().to_string())
        ));
        s.push_str("  </skill>\n");
    }
    s.push_str("</available_skills>");
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
