//! Extra coverage for pi-coding-agent context.rs helpers.

use pi_coding_agent::context;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ── settings_path ─────────────────────────────────────────────────────────────

#[test]
fn settings_path_equals_settings_paths_first() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let sp = context::settings_path();
    let (global, _) = context::settings_paths();
    assert_eq!(sp, global);
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

// ── skills_dirs contains .agents/skills and .pi/skills ───────────────────────

#[test]
fn skills_dirs_contains_project_level_entries() {
    let s = context::skills_dirs();
    let has_agents = s.iter().any(|p| p.ends_with(".agents/skills") || p.to_string_lossy().contains(".agents"));
    let has_pi = s.iter().any(|p| p.ends_with(".pi/skills") || p.to_string_lossy().contains(".pi"));
    assert!(has_agents || has_pi, "skills_dirs should include at least one project-level dir; got: {s:?}");
}

// ── prompts_dirs ──────────────────────────────────────────────────────────────

#[test]
fn prompts_dirs_contains_two_entries() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let dirs = context::prompts_dirs();
    assert_eq!(dirs.len(), 2, "prompts_dirs should return exactly 2 paths");
    assert!(dirs[0].starts_with(tmp.path()), "first prompts dir should be under agent_dir");
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

// ── themes_dirs ───────────────────────────────────────────────────────────────

#[test]
fn themes_dirs_contains_two_entries() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let dirs = context::themes_dirs();
    assert_eq!(dirs.len(), 2, "themes_dirs should return exactly 2 paths");
    assert!(dirs[0].starts_with(tmp.path()), "first themes dir should be under agent_dir");
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

// ── system_prompt_paths ────────────────────────────────────────────────────────

#[test]
fn system_prompt_paths_contains_two_entries() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let paths = context::system_prompt_paths();
    assert_eq!(paths.len(), 2, "system_prompt_paths should return exactly 2 paths");
    assert!(paths[0].ends_with("SYSTEM.md"), "first system prompt path should end with SYSTEM.md");
    std::env::remove_var("PI_CODING_AGENT_DIR");
}
