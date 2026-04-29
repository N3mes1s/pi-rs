use pi_orchestrate::{parse_campaign, validate};

const MINIMAL_TOML: &str = r#"
name           = "RFD 0020 v1.1 — autonomous router"
description    = "Three router milestones: static, classifier, stats."
target_branch  = "main"

[defaults]
reviewer       = "code-reviewer"
fix_loop_max   = 2
push_retry_max = 3

[[milestones]]
id           = "m1-static"
branch       = "claude/router-static"
depends_on   = []
assignment   = "Add pub mod router; ..."
implementer  = "router-implementer"

[[milestones.override_rules]]
match      = "(?i)integration test|e2e"
verdict    = "out-of-scope"
forward_to = "m2-classifier"

[[milestones]]
id          = "m2-classifier"
branch      = "claude/router-classifier"
depends_on  = ["m1-static"]
implementer = "router-implementer"
assignment  = "Implement the classifier..."

[[milestones]]
id          = "m3-stats"
branch      = "claude/router-stats"
depends_on  = ["m1-static"]
implementer = "router-implementer"
assignment  = "Add stats..."
"#;

#[test]
fn parse_minimal_campaign() {
    let campaign = parse_campaign(MINIMAL_TOML).expect("should parse");

    assert_eq!(campaign.name, "RFD 0020 v1.1 — autonomous router");
    assert_eq!(campaign.target_branch, "main");
    assert_eq!(campaign.milestones.len(), 3);

    assert_eq!(campaign.milestones[0].id, "m1-static");
    assert!(campaign.milestones[0].depends_on.is_empty());
    assert_eq!(campaign.milestones[0].override_rules.len(), 1);
    assert_eq!(
        campaign.milestones[0].override_rules[0].forward_to.as_deref(),
        Some("m2-classifier")
    );

    assert_eq!(campaign.milestones[1].id, "m2-classifier");
    assert_eq!(campaign.milestones[1].depends_on, vec!["m1-static"]);

    assert_eq!(campaign.milestones[2].id, "m3-stats");
    assert_eq!(campaign.milestones[2].depends_on, vec!["m1-static"]);
}

#[test]
fn validate_minimal_campaign_passes() {
    let campaign = parse_campaign(MINIMAL_TOML).expect("should parse");
    validate(&campaign).expect("should validate");
}

#[test]
fn defaults_applied_to_milestones() {
    let campaign = parse_campaign(MINIMAL_TOML).expect("should parse");

    // m1-static inherits defaults
    let m = &campaign.milestones[0];
    assert_eq!(m.effective_reviewer(&campaign.defaults), "code-reviewer");
    assert_eq!(m.effective_fix_loop_max(&campaign.defaults), 2);
}
