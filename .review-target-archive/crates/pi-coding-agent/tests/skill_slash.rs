//! A3: `/skill:<name>` explicit invocation.

use pi_coding_agent::modes::expand_slash_with;
use pi_coding_agent::skills::{Skill, SkillRegistry};
use std::path::PathBuf;

fn registry_with(name: &str, body: &str) -> SkillRegistry {
    let mut reg = SkillRegistry::new();
    reg.add(Skill {
        name: name.into(),
        description: "test skill".into(),
        body: body.into(),
        path: PathBuf::from(format!("/tmp/{name}/SKILL.md")),
    });
    reg
}

#[test]
fn skill_slash_injects_body_and_args() {
    let reg = registry_with("autoresearch-create", "BODY: do research\n");
    let out = expand_slash_with("/skill:autoresearch-create some goal", &reg);
    assert!(out.contains("# Skill: autoresearch-create"));
    assert!(out.contains("BODY: do research"));
    assert!(out.ends_with("some goal"));
    assert!(out.contains("---"));
}

#[test]
fn skill_slash_no_args_omits_separator() {
    let reg = registry_with("foo", "BODY\n");
    let out = expand_slash_with("/skill:foo", &reg);
    assert!(out.contains("# Skill: foo"));
    assert!(out.contains("BODY"));
    assert!(!out.contains("---"));
}

#[test]
fn skill_slash_unknown_passes_through() {
    let reg = SkillRegistry::new();
    let raw = "/skill:does-not-exist hello";
    let out = expand_slash_with(raw, &reg);
    // Unknown skill → echo the original prompt unchanged.
    assert_eq!(out, raw);
}

#[test]
fn non_skill_slash_unchanged() {
    let reg = SkillRegistry::new();
    assert_eq!(expand_slash_with("/help", &reg), "/help");
    assert_eq!(expand_slash_with("hello world", &reg), "hello world");
}

#[test]
fn autoresearch_slash_still_expands() {
    let reg = SkillRegistry::new();
    let out = expand_slash_with("/autoresearch optimise foo", &reg);
    assert_eq!(out, "autoresearch: optimise foo");
}
