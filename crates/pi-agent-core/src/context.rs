use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

/// Discovers AGENTS.md / CLAUDE.md context files: global config dir, then
/// cwd and all parents (closest takes precedence in pi but we just include
/// all of them and let the prompt order itself).
pub fn discover_context_files(cwd: &Path, agent_dir: &Path, names: &[&str]) -> Vec<ContextFile> {
    let mut out = Vec::new();
    for name in names {
        let p = agent_dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&p) {
            out.push(ContextFile { path: p, content });
        }
    }
    let mut ancestors: Vec<&Path> = cwd.ancestors().collect();
    ancestors.reverse();
    for dir in ancestors {
        for name in names {
            let p = dir.join(name);
            if p.is_file() {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    out.push(ContextFile { path: p, content });
                }
            }
        }
    }
    out
}
