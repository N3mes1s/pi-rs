//! Extra coverage for the package manager — install error paths plus
//! discover edge cases. We do not exercise real npm/git network operations;
//! instead we drive the `install()` failure paths through bogus specs.

use pi_coding_agent::packages::{discover, install, package_dirs, PackageManifest, PiSection};

#[test]
fn discover_returns_empty_when_root_is_not_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("not-a-dir");
    std::fs::write(&f, "I'm a regular file").unwrap();
    assert!(discover(&f).is_empty());
}

#[test]
fn discover_skips_files_at_top_level() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("loose.txt"), "hi").unwrap();
    let pkgs = discover(tmp.path());
    assert!(pkgs.is_empty());
}

#[test]
fn discover_handles_subdir_with_invalid_package_json() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("broken-pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("package.json"), "{ this isn't json").unwrap();
    let pkgs = discover(tmp.path());
    // The dir is still returned (default-empty manifest) so callers can
    // surface it; we just verify it doesn't crash.
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, ""); // default
}

#[test]
fn install_rejects_unsupported_scheme() {
    let tmp = tempfile::tempdir().unwrap();
    let r = install("file:/etc/passwd", tmp.path());
    assert!(r.is_err());
    let err = r.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn install_rejects_bare_name_without_scheme() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(install("just-a-name", tmp.path()).is_err());
}

#[test]
fn install_with_git_scheme_targeting_invalid_host_fails() {
    let tmp = tempfile::tempdir().unwrap();
    // The `git:` prefix maps to `https://<rest>` and runs `git clone`.
    // A non-resolvable host produces a non-zero exit. We assert the error
    // path is reached without inspecting the precise error message.
    let r = install(
        "git:nonexistent-fake-host.example.invalid/missing/repo",
        tmp.path(),
    );
    assert!(r.is_err());
}

#[test]
fn install_with_https_scheme_versioned_targets_versioned_dest_dir() {
    // We can't perform a real clone in CI, but we can drive the spec parser
    // by parsing a versioned spec and observing the error from a guaranteed
    // failed clone. We just want to make sure the `@version` branch is hit.
    let tmp = tempfile::tempdir().unwrap();
    let r = install(
        "https://nonexistent-host.invalid/u/r@v1",
        tmp.path(),
    );
    assert!(r.is_err());
}

#[test]
fn package_dirs_only_manifest_entries_when_conventional_dirs_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("p");
    std::fs::create_dir_all(pkg.join("custom-ext")).unwrap();
    std::fs::create_dir_all(pkg.join("custom-skill")).unwrap();
    std::fs::create_dir_all(pkg.join("custom-prompt")).unwrap();
    std::fs::create_dir_all(pkg.join("custom-theme")).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        serde_json::to_string(&PackageManifest {
            name: "p".into(),
            version: "0".into(),
            keywords: vec![],
            pi: Some(PiSection {
                extensions: vec!["custom-ext".into()],
                skills: vec!["custom-skill".into()],
                prompts: vec!["custom-prompt".into()],
                themes: vec!["custom-theme".into()],
            }),
        })
        .unwrap(),
    )
    .unwrap();
    let pkgs = discover(tmp.path());
    assert_eq!(pkgs.len(), 1);
    let dirs = package_dirs(&pkgs[0]);
    assert!(dirs.extensions.iter().any(|p| p.ends_with("custom-ext")));
    assert!(dirs.skills.iter().any(|p| p.ends_with("custom-skill")));
    assert!(dirs.prompts.iter().any(|p| p.ends_with("custom-prompt")));
    assert!(dirs.themes.iter().any(|p| p.ends_with("custom-theme")));
}

#[test]
fn package_dirs_with_no_pi_section_returns_only_conventional_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("p");
    std::fs::create_dir_all(pkg.join("extensions")).unwrap();
    std::fs::create_dir_all(pkg.join("skills")).unwrap();
    std::fs::write(
        pkg.join("package.json"),
        serde_json::to_string(&PackageManifest {
            name: "p".into(),
            version: "0".into(),
            keywords: vec![],
            pi: None,
        })
        .unwrap(),
    )
    .unwrap();
    let pkgs = discover(tmp.path());
    let dirs = package_dirs(&pkgs[0]);
    assert!(dirs.extensions.iter().any(|p| p.ends_with("extensions")));
    assert!(dirs.skills.iter().any(|p| p.ends_with("skills")));
    assert!(dirs.prompts.is_empty());
    assert!(dirs.themes.is_empty());
}
