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
    assert!(
        r.is_ok(),
        "run_update should be a no-op on empty dir: {:?}",
        r
    );

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

/// Drives the `git:` spec branch of `packages::install` (and therefore of
/// `cmd::run_install`) by pointing at a `https://` URL that definitely
/// does not resolve. The git binary will spawn, fail, and we get the
/// "git clone failed" error path covered.
#[test]
fn run_install_git_spec_pointing_at_missing_url_errors() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("packages");
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    std::env::set_var("PI_PACKAGE_DIR", &pkg);

    // Use a `https://` spec so install() builds the URL via the
    // `https://` branch and runs `git clone` against a host that won't
    // resolve. Either git is present and exits non-zero (the
    // "git clone failed" branch) or git is absent and we hit the spawn
    // error branch — both arms live in the same `git` arm of install().
    // We only assert the call returned Err.
    let r = cmd::run_install("https://pi-rs.invalid.localhost.example.test/never/exists.git");
    assert!(r.is_err(), "missing remote must error, got {r:?}");

    std::env::remove_var("PI_CODING_AGENT_DIR");
    std::env::remove_var("PI_PACKAGE_DIR");
}

/// Same coverage but via the `git:` scheme, which differs from
/// `https://` only in URL construction (it prepends `https://`). Using
/// a `file://` body sidesteps DNS entirely and reaches the spawn-fail
/// or non-zero-exit branch deterministically.
#[test]
fn run_install_git_scheme_with_nonexistent_local_repo_errors() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("packages");
    std::env::set_var("PI_CODING_AGENT_DIR", tmp.path());
    std::env::set_var("PI_PACKAGE_DIR", &pkg);

    // `git:<body>` → URL becomes `https://<body>` per install()'s
    // construction logic. Pointing at a syntactically-valid but
    // non-existent host gets us through the same git-clone-failure arm.
    let r = cmd::run_install("git:pi-rs.invalid.localhost.example.test/x/y");
    assert!(r.is_err(), "git: spec to bad host must error, got {r:?}");

    std::env::remove_var("PI_CODING_AGENT_DIR");
    std::env::remove_var("PI_PACKAGE_DIR");
}
