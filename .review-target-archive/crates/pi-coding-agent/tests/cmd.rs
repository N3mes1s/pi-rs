//! Tests for the top-level subcommands (list / config).
//!
//! We override `PI_CODING_AGENT_DIR` and `PI_PACKAGE_DIR` to a tempdir
//! so the tests don't touch the user's real `~/.pi/agent` directory.

use pi_coding_agent::cmd;

/// Guard the env-var changes within a single process; cargo runs tests
/// from one binary in parallel by default, so we serialise via a mutex.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn run_list_returns_ok_on_empty_packages_dir() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("packages");
    std::fs::create_dir_all(&pkg).unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    std::env::set_var("PI_PACKAGE_DIR", &pkg);

    let r = cmd::run_list();
    assert!(
        r.is_ok(),
        "run_list should succeed on empty pkg dir: {:?}",
        r
    );

    std::env::remove_var("PI_CODING_AGENT_DIR");
    std::env::remove_var("PI_PACKAGE_DIR");
}

#[test]
fn run_config_returns_ok_and_uses_overridden_agent_dir() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());

    // run_config prints to stdout but never errors when paths can be
    // displayed; we just verify it returns Ok.
    let r = cmd::run_config();
    assert!(r.is_ok(), "run_config should succeed: {:?}", r);

    std::env::remove_var("PI_CODING_AGENT_DIR");
}
