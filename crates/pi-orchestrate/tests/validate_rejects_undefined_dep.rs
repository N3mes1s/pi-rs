use pi_orchestrate::{parse_campaign, validate, ValidationError};

const GHOST_DEP_TOML: &str = r#"
name          = "ghost dep test"
target_branch = "main"

[[milestones]]
id          = "m1"
branch      = "pi/m1"
depends_on  = ["ghost"]
assignment  = "..."
implementer = "agent"
"#;

#[test]
fn validate_rejects_undefined_dep() {
    let campaign = parse_campaign(GHOST_DEP_TOML).expect("should parse");
    let errs = validate(&campaign).expect_err("should have validation errors");

    let has_undefined = errs.iter().any(|e| {
        matches!(
            e,
            ValidationError::UndefinedDependency { dep, .. } if dep == "ghost"
        )
    });
    assert!(
        has_undefined,
        "expected UndefinedDependency for 'ghost', got: {:?}",
        errs
    );
}

#[test]
fn validate_rejects_undefined_dep_error_message() {
    let campaign = parse_campaign(GHOST_DEP_TOML).expect("should parse");
    let errs = validate(&campaign).expect_err("should have validation errors");

    let messages: Vec<String> = errs.iter().map(|e| e.to_string()).collect();
    assert!(
        messages.iter().any(|m| m.contains("ghost")),
        "error message should mention 'ghost', got: {:?}",
        messages
    );
}
