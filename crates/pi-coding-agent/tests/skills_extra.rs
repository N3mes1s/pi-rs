//! Extra coverage for the SkillRegistry — load_all, add, get-by-missing-name,
//! `name.md` files without a description, and subdirs that lack a SKILL.md.

use pi_coding_agent::skills::{Skill, SkillRegistry};
use std::path::PathBuf;

#[test]
fn load_all_walks_each_dir_in_the_provided_list() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(dir1.path().join("a.md"), "first body").unwrap();
    std::fs::write(dir2.path().join("b.md"), "second body").unwrap();

    let mut reg = SkillRegistry::new();
    reg.load_all(&[
        dir1.path().to_path_buf(),
        dir2.path().to_path_buf(),
        // Non-existent dir is silently skipped by load_dir.
        PathBuf::from("/this/does/not/exist/anywhere"),
    ]);

    let names = reg.names();
    assert!(names.contains(&"a".to_string()));
    assert!(names.contains(&"b".to_string()));
}

#[test]
fn bare_md_with_only_a_heading_yields_empty_description() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("only-heading.md"), "# Heading\n# Another\n").unwrap();
    let mut reg = SkillRegistry::new();
    reg.load_dir(dir.path());
    let s = reg.get("only-heading").expect("loaded");
    assert_eq!(s.description, "", "headings should not be picked as description");
}

#[test]
fn dir_without_skill_md_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("looks-like-a-skill");
    std::fs::create_dir_all(&sub).unwrap();
    // A README.md is *not* SKILL.md and lives inside a subdir → ignored.
    std::fs::write(sub.join("README.md"), "not a skill").unwrap();

    let mut reg = SkillRegistry::new();
    reg.load_dir(dir.path());
    assert!(
        reg.names().is_empty(),
        "dir without SKILL.md should be ignored, names: {:?}",
        reg.names()
    );
}

#[test]
fn skill_md_with_invalid_utf8_is_silently_skipped() {
    // Forces the `read_skill -> None` branch (read_to_string fails).
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("badbytes");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("SKILL.md"), [0xFFu8, 0xFE, 0x00, 0xC0]).unwrap();

    // Also test the bare-`name.md` branch with invalid UTF-8.
    std::fs::write(dir.path().join("garbage.md"), [0xFFu8, 0xFE, 0x00]).unwrap();

    let mut reg = SkillRegistry::new();
    reg.load_dir(dir.path());
    assert!(reg.get("badbytes").is_none());
    assert!(reg.get("garbage").is_none());
}

#[test]
fn add_inserts_or_replaces_a_skill_by_name() {
    let mut reg = SkillRegistry::new();
    reg.add(Skill {
        name: "manual".into(),
        description: "first".into(),
        body: "body".into(),
        path: PathBuf::from("/tmp/x.md"),
    });
    assert_eq!(reg.get("manual").unwrap().description, "first");

    reg.add(Skill {
        name: "manual".into(),
        description: "second".into(),
        body: "body2".into(),
        path: PathBuf::from("/tmp/x.md"),
    });
    assert_eq!(reg.get("manual").unwrap().description, "second");
    assert!(reg.get("absent").is_none());
}

#[test]
fn load_dir_silently_returns_for_missing_directory() {
    let mut reg = SkillRegistry::new();
    reg.load_dir(std::path::Path::new("/this/path/should/not/exist/sk"));
    assert!(reg.names().is_empty());
}
