//! Extra coverage for the top-level subcommands (`install`, `update`).

use pi_coding_agent::cmd;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn run_install_with_unsupported_spec_returns_error() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("packages");
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    std::env::set_var("PI_PACKAGE_DIR", &pkg);

    let r = cmd::run_install("not-a-real-scheme:foo");
    assert!(r.is_err(), "bogus spec must error: {:?}", r);

    std::env::remove_var("PI_CODING_AGENT_DIR");
    std::env::remove_var("PI_PACKAGE_DIR");
}

#[test]
fn run_update_on_empty_packages_dir_succeeds() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("packages");
    std::fs::create_dir_all(&pkg).unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    std::env::set_var("PI_PACKAGE_DIR", &pkg);

    let r = cmd::run_update();
    assert!(r.is_ok(), "run_update should be a no-op on empty dir: {:?}", r);

    std::env::remove_var("PI_CODING_AGENT_DIR");
    std::env::remove_var("PI_PACKAGE_DIR");
}

#[test]
fn run_update_on_missing_packages_dir_still_succeeds() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    let pkg = tmp.path().join("missing-pkgs");
    std::env::set_var("PI_PACKAGE_DIR", &pkg);

    let r = cmd::run_update();
    assert!(r.is_ok());

    std::env::remove_var("PI_CODING_AGENT_DIR");
    std::env::remove_var("PI_PACKAGE_DIR");
}
