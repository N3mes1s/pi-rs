use pi_coding_agent::packages::{discover, package_dirs, PackageManifest, PiSection};

fn write_manifest(dir: &std::path::Path, manifest: &PackageManifest) {
    std::fs::write(
        dir.join("package.json"),
        serde_json::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn discover_reads_package_json_manifests_in_each_subdir() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("alpha-pkg");
    let b = root.path().join("beta-pkg");
    let nope = root.path().join("just-a-dir-no-manifest");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::create_dir_all(&nope).unwrap();

    write_manifest(
        &a,
        &PackageManifest {
            name: "alpha".into(),
            version: "0.1.0".into(),
            keywords: vec!["pi".into()],
            pi: Some(PiSection {
                extensions: vec!["my-ext".into()],
                ..Default::default()
            }),
        },
    );
    write_manifest(
        &b,
        &PackageManifest {
            name: "beta".into(),
            version: "0.2.0".into(),
            keywords: vec![],
            pi: None,
        },
    );

    let pkgs = discover(root.path());
    let names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "got: {names:?}");
    assert!(names.contains(&"beta"));
    // The dir without a manifest still appears (with empty name) per current
    // discover() semantics.
    assert_eq!(pkgs.len(), 3);
}

#[test]
fn package_dirs_returns_manifest_and_conventional_directories() {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();

    // Create the manifest-declared "my-ext" directory.
    std::fs::create_dir_all(pkg.join("my-ext")).unwrap();
    // Create conventional dirs.
    std::fs::create_dir_all(pkg.join("extensions")).unwrap();
    std::fs::create_dir_all(pkg.join("skills")).unwrap();
    std::fs::create_dir_all(pkg.join("prompts")).unwrap();
    std::fs::create_dir_all(pkg.join("themes")).unwrap();

    write_manifest(
        &pkg,
        &PackageManifest {
            name: "pkg".into(),
            version: "1.0.0".into(),
            keywords: vec![],
            pi: Some(PiSection {
                extensions: vec!["my-ext".into()],
                skills: vec![],
                prompts: vec![],
                themes: vec![],
            }),
        },
    );

    let pkgs = discover(root.path());
    assert_eq!(pkgs.len(), 1);
    let dirs = package_dirs(&pkgs[0]);

    // Manifest entry first, conventional `extensions/` second.
    assert!(dirs.extensions.iter().any(|p| p.ends_with("my-ext")));
    assert!(dirs.extensions.iter().any(|p| p.ends_with("extensions")));
    assert!(dirs.skills.iter().any(|p| p.ends_with("skills")));
    assert!(dirs.prompts.iter().any(|p| p.ends_with("prompts")));
    assert!(dirs.themes.iter().any(|p| p.ends_with("themes")));
}

#[test]
fn package_dirs_does_not_duplicate_conventional_dir_when_manifest_lists_it() {
    let root = tempfile::tempdir().unwrap();
    let pkg = root.path().join("pkg");
    std::fs::create_dir_all(pkg.join("extensions")).unwrap();
    write_manifest(
        &pkg,
        &PackageManifest {
            name: "pkg".into(),
            version: "1.0.0".into(),
            keywords: vec![],
            pi: Some(PiSection {
                extensions: vec!["extensions".into()],
                ..Default::default()
            }),
        },
    );
    let pkgs = discover(root.path());
    let dirs = package_dirs(&pkgs[0]);
    let count = dirs.extensions.iter().filter(|p| p.ends_with("extensions")).count();
    assert_eq!(count, 1, "conventional dir must not duplicate manifest entry");
}
