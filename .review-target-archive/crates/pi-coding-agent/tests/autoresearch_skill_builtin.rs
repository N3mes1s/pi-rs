//! Confirms the upstream `autoresearch-create` SKILL.md ships built-in.

use pi_coding_agent::skills::{ensure_builtin_skills_dir, SkillRegistry};

#[test]
fn builtin_autoresearch_create_skill_loads() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("PI_BUILTIN_SKILLS_DIR", dir.path());

    let base = ensure_builtin_skills_dir().unwrap();
    let skill_md = base.join("autoresearch-create").join("SKILL.md");
    assert!(skill_md.is_file(), "autoresearch-create/SKILL.md missing");
    let body = std::fs::read_to_string(&skill_md).unwrap();
    assert!(body.contains("autoresearch-create"));
    assert!(body.contains("init_experiment"));
    assert!(body.contains("run_experiment"));
    assert!(body.contains("log_experiment"));
    assert!(body.contains("autoresearch.md"));
    assert!(body.contains("autoresearch.sh"));

    let mut reg = SkillRegistry::new();
    reg.load_dir(&base.join("autoresearch-create"));
    assert!(
        reg.get("SKILL").is_some() || reg.get("autoresearch-create").is_some(),
        "skill not registered, names: {:?}",
        reg.names()
    );

    std::env::remove_var("PI_BUILTIN_SKILLS_DIR");
}
