use pi_orchestrate::{parse_campaign, validate, ValidationError};

const CYCLE_TOML: &str = r#"
name          = "cycle test"
target_branch = "main"

[[milestones]]
id          = "m1"
branch      = "pi/m1"
depends_on  = ["m2"]
assignment  = "..."
implementer = "agent"

[[milestones]]
id          = "m2"
branch      = "pi/m2"
depends_on  = ["m1"]
assignment  = "..."
implementer = "agent"
"#;

#[test]
fn validate_rejects_cycles() {
    let campaign = parse_campaign(CYCLE_TOML).expect("should parse");
    let errs = validate(&campaign).expect_err("should have errors");

    let has_cycle = errs.iter().any(|e| {
        let msg = e.to_string().to_lowercase();
        msg.contains("cycle")
    });
    assert!(has_cycle, "expected a cycle error, got: {:?}", errs);
}

#[test]
fn cycle_error_mentions_involved_milestone() {
    let campaign = parse_campaign(CYCLE_TOML).expect("should parse");
    let errs = validate(&campaign).expect_err("should have errors");

    let cycle_errors: Vec<_> = errs
        .iter()
        .filter_map(|e| match e {
            ValidationError::DependencyCycle(id) => Some(id.clone()),
            _ => None,
        })
        .collect();

    assert!(!cycle_errors.is_empty(), "expected DependencyCycle variant");
    // The cycle involves m1 and m2; the error should mention one of them.
    let mentioned = cycle_errors.iter().any(|id| id == "m1" || id == "m2");
    assert!(mentioned, "cycle error should mention m1 or m2, got: {:?}", cycle_errors);
}
