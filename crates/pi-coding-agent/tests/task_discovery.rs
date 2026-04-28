//! RFD 0005 test plan #1 — discovery precedence.
//!
//! Project-local `<repo>/.pi/agents/foo.md` MUST win over the user-level
//! `~/.pi/agent/agents/foo.md`. Also asserts that omitting `tools:`
//! yields an `AgentDefinition` with `tools.is_empty()`.

use pi_coding_agent::native::task::{discovery, AgentSource};
use std::fs;

const AGENT: &str = r#"---
name: code-reviewer
description: SOURCE-MARKER
---
hello world
"#;

#[test]
fn project_agent_wins_over_user() {
    let user_root = tempfile::tempdir().unwrap();
    let project_root = tempfile::tempdir().unwrap();

    // user agent.
    let user_agents = user_root.path().join("agents");
    fs::create_dir_all(&user_agents).unwrap();
    fs::write(
        user_agents.join("code-reviewer.md"),
        AGENT.replace("SOURCE-MARKER", "from-user"),
    )
    .unwrap();

    // project agent.
    let project_agents = project_root.path().join(".pi").join("agents");
    fs::create_dir_all(&project_agents).unwrap();
    fs::write(
        project_agents.join("code-reviewer.md"),
        AGENT.replace("SOURCE-MARKER", "from-project"),
    )
    .unwrap();

    // Point user-discovery at our temp.
    std::env::set_var("PI_CODING_AGENT_DIR", user_root.path());

    let agents = discovery::load_all(project_root.path());
    let cr = agents
        .iter()
        .find(|a| a.name == "code-reviewer")
        .expect("code-reviewer should be discovered");
    assert_eq!(cr.source, AgentSource::Project);
    assert_eq!(cr.description, "from-project");
    // No `tools:` in frontmatter ⇒ empty allowlist.
    assert!(cr.tools.is_empty());

    std::env::remove_var("PI_CODING_AGENT_DIR");
}

#[test]
fn user_agent_present_when_no_project() {
    let user_root = tempfile::tempdir().unwrap();
    let project_root = tempfile::tempdir().unwrap();
    let user_agents = user_root.path().join("agents");
    fs::create_dir_all(&user_agents).unwrap();
    fs::write(
        user_agents.join("solo.md"),
        AGENT
            .replace("code-reviewer", "solo")
            .replace("SOURCE-MARKER", "user-only"),
    )
    .unwrap();

    std::env::set_var("PI_CODING_AGENT_DIR", user_root.path());
    let agents = discovery::load_all(project_root.path());
    let solo = agents.iter().find(|a| a.name == "solo").unwrap();
    assert_eq!(solo.source, AgentSource::User);
    std::env::remove_var("PI_CODING_AGENT_DIR");
}
