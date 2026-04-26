//! Extra coverage for the prompts registry.

use pi_coding_agent::prompts::{PromptRegistry, PromptTemplate};
use std::path::PathBuf;

#[test]
fn load_all_walks_each_dir_and_skips_missing_dirs() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(dir1.path().join("first.md"), "x").unwrap();
    std::fs::write(dir2.path().join("second.md"), "y").unwrap();
    let mut reg = PromptRegistry::new();
    reg.load_all(&[
        dir1.path().to_path_buf(),
        dir2.path().to_path_buf(),
        PathBuf::from("/no/such/dir"),
    ]);
    let names = reg.names();
    assert!(names.contains(&"first".to_string()));
    assert!(names.contains(&"second".to_string()));
}

#[test]
fn load_dir_skips_files_without_a_stem() {
    let dir = tempfile::tempdir().unwrap();
    // Hidden-style file `.md` whose file_stem() is `.md` actually — let's
    // explicitly test a dotfile to ensure we don't crash regardless.
    std::fs::write(dir.path().join(".md"), "body").unwrap();
    let mut reg = PromptRegistry::new();
    reg.load_dir(dir.path());
    // No assertion on whether `.md` becomes a name — just don't panic.
    let _ = reg.names();
}

#[test]
fn add_inserts_or_replaces_a_template() {
    let mut reg = PromptRegistry::new();
    reg.add(PromptTemplate {
        name: "p".into(),
        body: "first".into(),
        path: PathBuf::from("/tmp/p.md"),
    });
    reg.add(PromptTemplate {
        name: "p".into(),
        body: "second".into(),
        path: PathBuf::from("/tmp/p.md"),
    });
    assert_eq!(reg.get("p").unwrap().body, "second");
    assert!(reg.get("absent").is_none());
}

#[test]
fn render_with_no_vars_leaves_template_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.md"), "no placeholders").unwrap();
    let mut reg = PromptRegistry::new();
    reg.load_dir(dir.path());
    let t = reg.get("plain").unwrap();
    let out = t.render(&std::collections::HashMap::new());
    assert_eq!(out, "no placeholders");
}
