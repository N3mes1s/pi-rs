//! RFD 0005 test plan #2 — definition parsing.
//!
//! Round-trip the `code-reviewer` example from the RFD, and confirm
//! that `deny_unknown_fields` plus required fields actually reject
//! malformed agents.

use pi_coding_agent::native::task::definition::{AgentDefinition, SpawnsRule};

const CODE_REVIEWER: &str = r#"---
name: code-reviewer
description: Reviews a diff for correctness, security, and style.
tools: [read, grep, find, bash, lsp]
spawns: explore
model: pi/slow
thinking: medium
output:
  properties:
    overall_correctness: { enum: [correct, incorrect] }
    notes: { type: string }
---
You are a senior reviewer. Read the diff at $DIFF_PATH and produce a
verdict using only the read-only tools available to you.
"#;

#[test]
fn parses_rfd_example() {
    let def = AgentDefinition::parse(CODE_REVIEWER).expect("parse ok");
    assert_eq!(def.name, "code-reviewer");
    assert_eq!(
        def.description,
        "Reviews a diff for correctness, security, and style."
    );
    assert_eq!(def.tools, vec!["read", "grep", "find", "bash", "lsp"]);
    match def.spawns.as_ref().unwrap() {
        SpawnsRule::Named(v) => assert_eq!(v, &vec!["explore".to_string()]),
        other => panic!("unexpected spawns: {other:?}"),
    }
    assert_eq!(def.model.as_deref(), Some("pi/slow"));
    assert_eq!(def.thinking.as_deref(), Some("medium"));
    assert!(def.output.is_some());
    assert!(def.system_prompt.starts_with("You are a senior reviewer."));
}

#[test]
fn missing_name_is_rejected() {
    let s = "---\ndescription: x\n---\nbody\n";
    assert!(AgentDefinition::parse(s).is_err());
}

#[test]
fn missing_description_is_rejected() {
    let s = "---\nname: x\n---\nbody\n";
    assert!(AgentDefinition::parse(s).is_err());
}

#[test]
fn unknown_field_is_rejected() {
    let s = "---\nname: x\ndescription: y\nbogus: 1\n---\nbody\n";
    assert!(AgentDefinition::parse(s).is_err());
}

#[test]
fn missing_frontmatter_is_rejected() {
    let s = "no frontmatter here\n";
    assert!(AgentDefinition::parse(s).is_err());
}

#[test]
fn spawns_star_means_all() {
    let s = "---\nname: x\ndescription: y\nspawns: \"*\"\n---\nb\n";
    let def = AgentDefinition::parse(s).unwrap();
    assert!(matches!(def.spawns, Some(SpawnsRule::All)));
}

#[test]
fn spawns_list_round_trips() {
    let s = "---\nname: x\ndescription: y\nspawns: [a, b, c]\n---\nb\n";
    let def = AgentDefinition::parse(s).unwrap();
    match def.spawns.unwrap() {
        SpawnsRule::Named(v) => assert_eq!(v, vec!["a", "b", "c"]),
        other => panic!("unexpected: {other:?}"),
    }
}
