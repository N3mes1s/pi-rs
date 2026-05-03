//! End-to-end codegen tests per RFD 0028 §B.14.
//!
//! Exercises the full `parse → render → write_tree` pipeline
//! against fixture manifests. Cargo-build smoke is in a separate
//! cfg-gated test (build_smoke.rs) so the test fleet runs fast
//! by default.

use pi_build::{parse, render, write_tree, BuildOptions, PI_BUILD_VERSION};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(rel: &str) -> String {
    let path = format!("{FIXTURES}/{rel}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

#[test]
fn dice_oracle_render_writes_three_files() {
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
    };
    write_tree(&tree, &opts).expect("write");

    assert!(opts.out_dir.join("Cargo.toml").is_file());
    assert!(opts.out_dir.join("src/main.rs").is_file());
    assert!(opts.out_dir.join("pi-build.lock").is_file());
}

#[test]
fn determinism_two_runs_identical_bytes() {
    // B.13 invariant 5.
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let a = render(&m, &raw, PI_BUILD_VERSION);
    let b = render(&m, &raw, PI_BUILD_VERSION);
    assert_eq!(a, b, "two runs of the same input must produce identical bytes");
}

#[test]
fn determinism_minimal_defaults() {
    let raw = fixture("valid/minimal-defaults.toml");
    let m = parse(&raw).expect("parse");
    let a = render(&m, &raw, PI_BUILD_VERSION);
    let b = render(&m, &raw, PI_BUILD_VERSION);
    assert_eq!(a, b);
}

#[test]
fn lock_file_records_correct_sha() {
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let tree = render(&m, &raw, PI_BUILD_VERSION);
    let expected_sha = pi_build::manifest_sha256(&raw);
    assert!(
        tree.pi_build_lock.contains(&format!("manifest_sha256  = \"{expected_sha}\"")),
        "lock file: {}",
        tree.pi_build_lock,
    );
}

#[test]
fn rendered_main_rs_is_valid_rust_syntax() {
    // Parse the rendered main.rs through syn — catches all kinds
    // of "looks fine but doesn't tokenize" template bugs.
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse");
    let tree = render(&m, &raw, PI_BUILD_VERSION);
    syn::parse_file(&tree.main_rs).expect("rendered main.rs must be valid Rust");
}

#[test]
fn rendered_main_rs_for_minimal_defaults_is_valid_rust() {
    let raw = fixture("valid/minimal-defaults.toml");
    let m = parse(&raw).expect("parse");
    let tree = render(&m, &raw, PI_BUILD_VERSION);
    syn::parse_file(&tree.main_rs).expect("minimal main.rs must be valid Rust");
}
