//! Smoke-coverage for the small `context.rs` helpers — these resolve the
//! per-user `~/.pi/agent` directory, the per-project `.pi/` directory, and
//! the various JSON file paths beneath them.

use pi_coding_agent::context;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn agent_dir_honours_pi_coding_agent_dir_env_var() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let dir = context::agent_dir();
    assert_eq!(dir, tmp.path());
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

#[test]
fn agent_dir_falls_back_to_home_dot_pi_agent() {
    let _g = lock();
    std::env::remove_var("PI_CODING_AGENT_DIR");
    let dir = context::agent_dir();
    assert!(dir.ends_with(".pi/agent") || dir.ends_with(".pi\\agent"));
}

#[test]
fn package_dir_honours_pi_package_dir_env_var() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_PACKAGE_DIR", tmp.path());
    assert_eq!(context::package_dir(), tmp.path());
    std::env::remove_var("PI_PACKAGE_DIR");
}

#[test]
fn package_dir_defaults_below_agent_dir() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    std::env::remove_var("PI_PACKAGE_DIR");
    assert_eq!(context::package_dir(), tmp.path().join("packages"));
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

#[test]
fn project_dir_returns_dot_pi() {
    assert_eq!(context::project_dir(), std::path::PathBuf::from(".pi"));
}

#[test]
fn settings_paths_returns_global_and_project_paths() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let (g, p) = context::settings_paths();
    assert_eq!(g, tmp.path().join("settings.json"));
    assert_eq!(p, std::path::PathBuf::from(".pi").join("settings.json"));
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

#[test]
fn auth_path_keybindings_path_sessions_dir_under_agent_dir() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    assert_eq!(context::auth_path(), tmp.path().join("auth.json"));
    assert_eq!(
        context::keybindings_path(),
        tmp.path().join("keybindings.json")
    );
    assert_eq!(context::sessions_dir(), tmp.path().join("sessions"));
    std::env::remove_var("PI_CODING_AGENT_DIR");
}

#[test]
fn skills_prompts_themes_and_system_prompt_dir_lists_are_non_empty() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());

    let s = context::skills_dirs();
    assert!(!s.is_empty());
    assert!(s.iter().any(|p| p.starts_with(tmp.path())));

    let p = context::prompts_dirs();
    assert!(p.iter().any(|p| p.starts_with(tmp.path())));

    let t = context::themes_dirs();
    assert!(t.iter().any(|p| p.starts_with(tmp.path())));

    let sp = context::system_prompt_paths();
    assert!(sp.iter().any(|p| p.starts_with(tmp.path())));

    std::env::remove_var("PI_CODING_AGENT_DIR");
}
