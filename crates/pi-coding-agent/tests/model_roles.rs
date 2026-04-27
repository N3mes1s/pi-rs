//! B1: model role routing. Tests parse-and-resolve plus ModelRoles JSON
//! round-tripping. CLI/env overrides are exercised via the `cli` parser.

use pi_agent_core::settings::{ModelRoles, Role, Settings};

#[test]
fn role_parse_accepts_canonical_names() {
    assert_eq!(Role::parse("default"), Some(Role::Default));
    assert_eq!(Role::parse("smol"), Some(Role::Smol));
    assert_eq!(Role::parse("slow"), Some(Role::Slow));
    assert_eq!(Role::parse("plan"), Some(Role::Plan));
    assert_eq!(Role::parse("commit"), Some(Role::Commit));
    // empty -> Default (matches /model role: with no arg).
    assert_eq!(Role::parse(""), Some(Role::Default));
    // case-insensitive
    assert_eq!(Role::parse("SMOL"), Some(Role::Smol));
    assert_eq!(Role::parse("Plan"), Some(Role::Plan));
    // unknown
    assert_eq!(Role::parse("nope"), None);
}

#[test]
fn resolve_falls_back_to_default_when_role_unset() {
    let r = ModelRoles {
        smol: Some("haiku".into()),
        ..Default::default()
    };
    assert_eq!(r.resolve(Role::Smol, "sonnet"), "haiku");
    assert_eq!(r.resolve(Role::Slow, "sonnet"), "sonnet");
    assert_eq!(r.resolve(Role::Plan, "sonnet"), "sonnet");
    assert_eq!(r.resolve(Role::Default, "sonnet"), "sonnet");
}

#[test]
fn resolve_default_role_uses_default_field_when_set() {
    let r = ModelRoles {
        default: Some("opus".into()),
        ..Default::default()
    };
    assert_eq!(r.resolve(Role::Default, "sonnet"), "opus");
}

#[test]
fn settings_roundtrip_includes_roles() {
    let mut s = Settings::default();
    s.roles.smol = Some("haiku".into());
    s.roles.plan = Some("opus".into());
    let j = serde_json::to_string(&s).unwrap();
    let back: Settings = serde_json::from_str(&j).unwrap();
    assert_eq!(back.roles.smol, Some("haiku".into()));
    assert_eq!(back.roles.plan, Some("opus".into()));
    assert_eq!(back.roles.slow, None);
}

#[test]
fn settings_default_has_empty_roles() {
    let s = Settings::default();
    assert_eq!(s.roles, ModelRoles::default());
    assert!(s.roles.smol.is_none());
}

#[test]
fn settings_loads_roles_block_from_json() {
    // simulate ~/.pi/agent/settings.json with a custom roles block.
    let raw = r#"{
        "provider": "anthropic",
        "model": "sonnet",
        "roles": {
            "smol": "haiku",
            "slow": "openai/o3-mini"
        }
    }"#;
    let s: Settings = serde_json::from_str(raw).unwrap();
    assert_eq!(s.roles.smol.as_deref(), Some("haiku"));
    assert_eq!(s.roles.slow.as_deref(), Some("openai/o3-mini"));
    assert!(s.roles.plan.is_none());
    assert!(s.roles.commit.is_none());
}

// CLI parsing: precedence is CLI > env > settings.
#[test]
fn cli_smol_flag_is_picked_up() {
    use clap::Parser;
    use pi_coding_agent::cli::Cli;
    let cli = Cli::try_parse_from(["pi", "--smol", "haiku"]).unwrap();
    assert_eq!(cli.smol.as_deref(), Some("haiku"));
}

#[test]
fn cli_slow_and_plan_flags_independent() {
    use clap::Parser;
    use pi_coding_agent::cli::Cli;
    let cli =
        Cli::try_parse_from(["pi", "--slow", "opus", "--plan", "sonnet-thinking"]).unwrap();
    assert_eq!(cli.slow.as_deref(), Some("opus"));
    assert_eq!(cli.plan.as_deref(), Some("sonnet-thinking"));
    assert!(cli.smol.is_none());
}
