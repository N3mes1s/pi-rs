use std::path::PathBuf;

/// Resolves the pi config directory: `$PI_CODING_AGENT_DIR` or `~/.pi/agent`.
pub fn agent_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PI_CODING_AGENT_DIR") {
        return PathBuf::from(p);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi").join("agent")
}

pub fn package_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PI_PACKAGE_DIR") {
        return PathBuf::from(p);
    }
    agent_dir().join("packages")
}

pub fn project_dir() -> PathBuf {
    PathBuf::from(".pi")
}

pub fn settings_paths() -> (PathBuf, PathBuf) {
    (agent_dir().join("settings.json"), project_dir().join("settings.json"))
}

/// Returns the global (user-level) settings.json path.
/// Equivalent to `settings_paths().0`.
pub fn settings_path() -> PathBuf {
    settings_paths().0
}

pub fn auth_path() -> PathBuf {
    agent_dir().join("auth.json")
}

pub fn keybindings_path() -> PathBuf {
    agent_dir().join("keybindings.json")
}

pub fn sessions_dir() -> PathBuf {
    agent_dir().join("sessions")
}

pub fn skills_dirs() -> Vec<PathBuf> {
    let mut out = vec![agent_dir().join("skills")];
    out.push(PathBuf::from(".agents").join("skills"));
    out.push(PathBuf::from(".pi").join("skills"));
    out
}

pub fn prompts_dirs() -> Vec<PathBuf> {
    vec![agent_dir().join("prompts"), PathBuf::from(".pi").join("prompts")]
}

pub fn themes_dirs() -> Vec<PathBuf> {
    vec![agent_dir().join("themes"), PathBuf::from(".pi").join("themes")]
}

pub fn system_prompt_paths() -> Vec<PathBuf> {
    vec![agent_dir().join("SYSTEM.md"), PathBuf::from(".pi").join("SYSTEM.md")]
}
