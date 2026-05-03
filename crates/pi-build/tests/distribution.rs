//! RFD 0028 Commit C distribution tests per §C.7.
//!
//! The build-related flag implementations (`--build`, `--target`,
//! `--release`/`--debug`, `--force`, `--out`) shipped in Commit B
//! alongside the codegen. Commit C adds the C.7 test cases that
//! Commit B left as TODO: pi-build.lock shape snapshot, no-extra-
//! flags cargo wrapping mock, and the --debug profile path.

use pi_build::{
    cargo_build, manifest_sha256, parse, render, write_tree, BuildError, BuildOptions,
    PI_BUILD_VERSION,
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(rel: &str) -> String {
    let path = format!("{FIXTURES}/{rel}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

// --- C.7 — pi-build.lock shape snapshot ---

#[test]
fn pi_build_lock_has_required_fields_and_correct_shape() {
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let tree = render(&m, &raw, PI_BUILD_VERSION);

    // Re-parse the lock as TOML and verify the schema shape.
    let parsed: toml::Value = toml::from_str(&tree.pi_build_lock)
        .expect("pi-build.lock must be valid TOML");
    let table = parsed.as_table().expect("top-level table");

    // Required fields per C.3.
    let ver = table
        .get("pi_build_version")
        .and_then(|v| v.as_str())
        .expect("pi_build_version");
    let sha = table
        .get("manifest_sha256")
        .and_then(|v| v.as_str())
        .expect("manifest_sha256");

    // pi_build_version parses as SemVer.
    semver::Version::parse(ver).expect("pi_build_version must be valid SemVer");

    // manifest_sha256 is 64 hex chars (SHA-256 hex output).
    assert_eq!(sha.len(), 64, "sha256 hex length");
    assert!(
        sha.chars().all(|c| c.is_ascii_hexdigit()),
        "sha256 must be hex: {sha}",
    );
    // And the recorded sha matches the actual manifest.
    assert_eq!(sha, manifest_sha256(&raw));

    // Per C.3 the lock file does NOT carry `generated_at_unix`
    // (would break determinism per §Cross-cutting #2).
    assert!(table.get("generated_at_unix").is_none());
}

#[test]
fn pi_build_lock_is_byte_identical_across_runs() {
    // Determinism per §Cross-cutting #2 + B.13 #5, applied to
    // the lock file specifically (the most likely candidate for
    // an accidental timestamp injection).
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let a = render(&m, &raw, PI_BUILD_VERSION);
    let b = render(&m, &raw, PI_BUILD_VERSION);
    assert_eq!(a.pi_build_lock, b.pi_build_lock);
}

// --- C.7 — `--debug` flips both the cargo invocation AND BuildOutcome.binary_path ---

#[tokio::test]
async fn release_profile_drives_release_argv_and_release_binary_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (cargo_path, recorder) = build_mock_cargo(tmp.path());
    let out_dir = tmp.path().join("agent-build");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Cargo.toml"), "").unwrap();

    let opts = BuildOptions {
        out_dir: out_dir.clone(),
        force: false,
        build: true,
        target: None,
        release: true,
        cargo_path: Some(cargo_path),
    };
    let outcome = cargo_build(&opts).await.expect("mock cargo exits 0");
    let argv = std::fs::read_to_string(&recorder).unwrap();
    assert!(argv.lines().any(|l| l == "--release"), "release flag forwarded: {argv}");
    assert!(
        outcome.binary_path.to_string_lossy().contains("/target/release/"),
        "binary_path must route through target/release/, got {}",
        outcome.binary_path.display(),
    );
}

#[tokio::test]
async fn debug_profile_drops_release_argv_and_routes_debug_binary_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (cargo_path, recorder) = build_mock_cargo(tmp.path());
    let out_dir = tmp.path().join("agent-build");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Cargo.toml"), "").unwrap();

    let opts = BuildOptions {
        out_dir: out_dir.clone(),
        force: false,
        build: true,
        target: None,
        release: false,
        cargo_path: Some(cargo_path),
    };
    let outcome = cargo_build(&opts).await.expect("mock cargo exits 0");
    let argv = std::fs::read_to_string(&recorder).unwrap();
    assert!(
        !argv.lines().any(|l| l == "--release"),
        "--release MUST NOT appear in --debug argv: {argv}",
    );
    assert!(
        outcome.binary_path.to_string_lossy().contains("/target/debug/"),
        "binary_path must route through target/debug/, got {}",
        outcome.binary_path.display(),
    );
}

// --- C.7 — no extraneous flags via wrapping cargo mock ---

/// Build a tiny shell script that records its argv to a sentinel
/// file and exits 0. We point pi-build's cargo invocation at this
/// script via `BuildOptions.cargo_path` (NOT PATH manipulation —
/// PATH is process-wide and races with parallel tests).
fn build_mock_cargo(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let recorder = tmp.join("recorded-argv.txt");
    let cargo_script = tmp.join("mock-cargo");
    let script = format!(
        "#!/bin/sh\nfor a; do printf '%s\\n' \"$a\"; done > '{}'\nexit 0\n",
        recorder.display()
    );
    std::fs::write(&cargo_script, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&cargo_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (cargo_script, recorder)
}

#[tokio::test]
async fn cargo_invocation_only_adds_manifest_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (cargo_path, recorder) = build_mock_cargo(tmp.path());
    let out_dir = tmp.path().join("agent-out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Cargo.toml"), "").unwrap();

    let opts = BuildOptions {
        out_dir: out_dir.clone(),
        force: false,
        build: true,
        target: None,
        release: true,
        cargo_path: Some(cargo_path),
    };
    cargo_build(&opts).await.expect("mock cargo should exit 0");

    let recorded = std::fs::read_to_string(&recorder).expect("argv recorded");
    let argv: Vec<&str> = recorded.lines().collect();
    assert_eq!(
        argv,
        vec![
            "build",
            "--release",
            "--manifest-path",
            out_dir.join("Cargo.toml").to_str().unwrap(),
        ],
        "unexpected cargo argv: {argv:?}",
    );
}

#[tokio::test]
async fn cargo_invocation_with_target_forwards_target_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let (cargo_path, recorder) = build_mock_cargo(tmp.path());
    let out_dir = tmp.path().join("agent-out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Cargo.toml"), "").unwrap();

    let opts = BuildOptions {
        out_dir: out_dir.clone(),
        force: false,
        build: true,
        target: Some("x86_64-unknown-linux-musl".into()),
        release: true,
        cargo_path: Some(cargo_path),
    };
    cargo_build(&opts).await.expect("mock cargo should exit 0");

    let recorded = std::fs::read_to_string(&recorder).expect("argv recorded");
    let argv: Vec<&str> = recorded.lines().collect();
    assert_eq!(
        argv,
        vec![
            "build",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
            "--manifest-path",
            out_dir.join("Cargo.toml").to_str().unwrap(),
        ],
        "unexpected cargo argv: {argv:?}",
    );
}

// --- C.7 — cargo failure → exit-mapping ---

#[tokio::test]
async fn cargo_nonzero_exit_returns_cargo_failed_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cargo_script = tmp.path().join("failing-cargo");
    std::fs::write(&cargo_script, "#!/bin/sh\nexit 101\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&cargo_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out_dir = tmp.path().join("agent-out");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("Cargo.toml"), "").unwrap();

    let opts = BuildOptions {
        out_dir,
        force: false,
        build: true,
        target: None,
        release: true,
        cargo_path: Some(cargo_script),
    };
    let err = cargo_build(&opts).await.expect_err("mock cargo exits 101");
    assert!(matches!(err, BuildError::CargoFailed(_)), "{err:?}");
}

// --- C.3.1 — write_tree non-empty + force matrix completeness ---
//
// Three of the four C.3.1 cases ship in the build module's unit
// tests (write_tree_creates_missing_dir, _empty_dir_succeeds,
// _non_empty_no_force_errors, _non_empty_with_force_wipes_then_writes).
// This file is the integration-level entry point that runs the
// full parse→render→write_tree pipeline against a real fixture.

#[test]
fn full_pipeline_writes_three_files_for_dice_oracle() {
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let tree = render(&m, &raw, PI_BUILD_VERSION);
    let tmp = tempfile::tempdir().unwrap();
    let opts = BuildOptions {
        out_dir: tmp.path().to_path_buf(),
        force: false,
        build: false,
        target: None,
        release: true,
        cargo_path: None,
    };
    write_tree(&tree, &opts).expect("write");

    // The three operator artifacts per §C.3.
    assert!(opts.out_dir.join("Cargo.toml").is_file());
    assert!(opts.out_dir.join("src/main.rs").is_file());
    assert!(opts.out_dir.join("pi-build.lock").is_file());
    // No `target/` directory yet (no --build).
    assert!(!opts.out_dir.join("target").exists());
}
