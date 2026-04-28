//! Walk the three discovery roots (bundled, user, project) and merge
//! the discovered [`AgentDefinition`]s with the precedence stated in
//! RFD 0005: `Project > User > Bundled`.

use std::path::{Path, PathBuf};

use super::definition::{AgentDefinition, AgentSource};

/// Project-local agents directory (`<repo>/.pi/agents/`).
pub fn project_agents_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".pi").join("agents")
}

/// User-level agents directory (`~/.pi/agent/agents/`).
pub fn user_agents_dir() -> PathBuf {
    crate::context::agent_dir().join("agents")
}

/// Load every `*.md` file under `dir` as an [`AgentDefinition`], tagging
/// each with `source`. Silently skips files that fail to parse — a
/// broken agent shouldn't take the binary down.
fn load_dir(dir: &Path, source: AgentSource) -> Vec<AgentDefinition> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(txt) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(mut def) = AgentDefinition::parse(&txt) else {
            continue;
        };
        def.source = source.clone();
        def.file_path = Some(p);
        out.push(def);
    }
    out
}

/// Load every agent from all three roots, applying precedence:
/// project overrides user overrides bundled. Bundled is empty for v1.
pub fn load_all(repo_root: &Path) -> Vec<AgentDefinition> {
    use std::collections::BTreeMap;

    let mut by_name: BTreeMap<String, AgentDefinition> = BTreeMap::new();

    // 1. Bundled (empty in v1; reserved for `include_dir!`).
    for def in load_bundled() {
        by_name.insert(def.name.clone(), def);
    }
    // 2. User.
    for def in load_dir(&user_agents_dir(), AgentSource::User) {
        by_name.insert(def.name.clone(), def);
    }
    // 3. Project (highest precedence).
    for def in load_dir(&project_agents_dir(repo_root), AgentSource::Project) {
        by_name.insert(def.name.clone(), def);
    }

    by_name.into_values().collect()
}

/// Bundled agents shipped with the binary. Empty for v1; reserved for a
/// future `include_dir!`-driven set of stock agents (code-reviewer,
/// explore, …).
fn load_bundled() -> Vec<AgentDefinition> {
    Vec::new()
}
