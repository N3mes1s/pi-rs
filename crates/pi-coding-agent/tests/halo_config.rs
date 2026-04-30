//! Integration tests for `halo::config` — covers every M1-tagged row from
//! RFD 0025 §Test plan / Unit tests.

use pi_coding_agent::halo::config;

// --------------------------------------------------------------------------
// 1. Minimum-valid halo.toml parses successfully.
// --------------------------------------------------------------------------
#[test]
fn parse_minimum_valid() {
    let toml_str = r#"
name = "test loop"

[clone]
expected_root = "~/work/repo*"
"#;
    let cfg = config::parse(toml_str).expect("should parse minimum-valid halo.toml");
    assert_eq!(cfg.name, "test loop");
    assert_eq!(cfg.target_branch, "halo/auto-merge"); // default
    assert_eq!(cfg.guardrails.daily_spend_budget_usd, 10.0); // default
    assert_eq!(cfg.orchestrate.per_cycle_overspend_threshold_usd, 4.0); // default
    let errors = config::validate(&cfg, false);
    assert!(errors.is_empty(), "expected no errors: {:?}", errors);
}

// --------------------------------------------------------------------------
// 2. #[serde(deny_unknown_fields)] — unknown field causes parse error.
// --------------------------------------------------------------------------
#[test]
fn reject_unknown_top_level_field() {
    let toml_str = r#"
name = "test"
made_up_field = "oh no"

[clone]
expected_root = "~/work/repo*"
"#;
    assert!(config::parse(toml_str).is_err(), "unknown field should fail parse");
}

#[test]
fn reject_unknown_guardrails_field() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[guardrails]
totally_invented = 99
"#;
    assert!(config::parse(toml_str).is_err(), "unknown guardrails field should fail parse");
}

#[test]
fn reject_unknown_orchestrate_field_parallel() {
    // v0.8: `parallel` removed from v1 schema; must be rejected.
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[orchestrate]
parallel = 4
"#;
    assert!(config::parse(toml_str).is_err(), "parallel field should be rejected by deny_unknown_fields");
}

// --------------------------------------------------------------------------
// 3. target_branch = "main" without --halo-allow-main is rejected.
// --------------------------------------------------------------------------
#[test]
fn reject_main_target_branch_without_allow_flag() {
    let toml_str = r#"
name = "danger"
target_branch = "main"

[clone]
expected_root = "~/work/repo*"
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false /* allow_main = false */);
    assert!(
        errors.iter().any(|e| e.contains("--halo-allow-main")),
        "expected error about --halo-allow-main, got: {:?}",
        errors
    );
}

#[test]
fn allow_main_with_flag() {
    let toml_str = r#"
name = "danger"
target_branch = "main"

[clone]
expected_root = "~/work/repo*"
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, true /* allow_main = true */);
    // no target_branch error; other errors may still exist
    assert!(
        !errors.iter().any(|e| e.contains("--halo-allow-main")),
        "expected no --halo-allow-main error when flag is set, got: {:?}",
        errors
    );
}

// --------------------------------------------------------------------------
// 4. auto_approve = "yolo" is rejected.
// --------------------------------------------------------------------------
#[test]
fn reject_yolo_auto_approve() {
    let toml_str = r#"
name = "danger"

[clone]
expected_root = "~/work/repo*"

[orchestrate]
auto_approve = "yolo"
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("yolo")),
        "expected error about yolo, got: {:?}",
        errors
    );
}

// --------------------------------------------------------------------------
// 5. Negative or NaN cost values are rejected.
// --------------------------------------------------------------------------
#[test]
fn reject_negative_daily_spend_budget() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[guardrails]
daily_spend_budget_usd = -5.0
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("daily_spend_budget_usd")),
        "expected error about daily_spend_budget_usd, got: {:?}",
        errors
    );
}

#[test]
fn reject_negative_per_cycle_threshold() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[orchestrate]
per_cycle_overspend_threshold_usd = -1.0
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("per_cycle_overspend_threshold_usd")),
        "expected error about per_cycle_overspend_threshold_usd, got: {:?}",
        errors
    );
}

// --------------------------------------------------------------------------
// 6. [cycle].steps that doesn't match canonical 8-step list is rejected.
// --------------------------------------------------------------------------
#[test]
fn reject_steps_wrong_order() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[cycle]
steps = [
  "synthesise_campaign",
  "pick_proposal",
  "prep_branch",
  "orchestrate",
  "keep_marker_scan",
  "smoke",
  "rollback_if_regress",
  "evolve_tick",
]
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("canonical eight-step list")),
        "expected canonical-step-list error, got: {:?}",
        errors
    );
}

#[test]
fn reject_steps_missing_entry() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[cycle]
steps = [
  "pick_proposal",
  "prep_branch",
  "orchestrate",
  "keep_marker_scan",
  "smoke",
  "rollback_if_regress",
  "evolve_tick",
]
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("canonical eight-step list")),
        "expected canonical-step-list error, got: {:?}",
        errors
    );
}

#[test]
fn reject_steps_evolve_tick_not_last() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[cycle]
steps = [
  "pick_proposal",
  "synthesise_campaign",
  "prep_branch",
  "evolve_tick",
  "orchestrate",
  "keep_marker_scan",
  "smoke",
  "rollback_if_regress",
]
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("canonical eight-step list")),
        "expected error since evolve_tick is not last, got: {:?}",
        errors
    );
}

#[test]
fn accept_canonical_steps() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[cycle]
steps = [
  "pick_proposal",
  "synthesise_campaign",
  "prep_branch",
  "orchestrate",
  "keep_marker_scan",
  "smoke",
  "rollback_if_regress",
  "evolve_tick",
]
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    // Only the clone.expected_root check should pass here (it's set).
    assert!(
        !errors.iter().any(|e| e.contains("canonical eight-step list")),
        "canonical step list should not produce an error, got: {:?}",
        errors
    );
}

// --------------------------------------------------------------------------
// 7. clone.expected_root absent → validation error.
// --------------------------------------------------------------------------
#[test]
fn reject_missing_expected_root() {
    let toml_str = r#"
name = "test"
"#;
    let cfg = config::parse(toml_str).unwrap();
    let errors = config::validate(&cfg, false);
    assert!(
        errors.iter().any(|e| e.contains("clone.expected_root not set")),
        "expected error about missing expected_root, got: {:?}",
        errors
    );
}

// --------------------------------------------------------------------------
// 8. v0.27 knob renames: daily_spend_budget_usd and per_cycle_overspend_threshold_usd.
//    The old names must NOT parse (deny_unknown_fields on guardrails / orchestrate).
// --------------------------------------------------------------------------
#[test]
fn reject_old_knob_name_daily_cost_cap_usd() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[guardrails]
daily_cost_cap_usd = 10.0
"#;
    // old name → unknown field → parse error
    assert!(config::parse(toml_str).is_err(), "old name daily_cost_cap_usd should be rejected");
}

#[test]
fn reject_old_knob_name_per_cycle_cost_cap_usd() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[orchestrate]
per_cycle_cost_cap_usd = 4.0
"#;
    assert!(config::parse(toml_str).is_err(), "old name per_cycle_cost_cap_usd should be rejected");
}

// --------------------------------------------------------------------------
// 9. model_override empty string is None, not Some("").
// --------------------------------------------------------------------------
#[test]
fn model_override_empty_string_is_none() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[proposer]
model_override = ""
"#;
    let cfg = config::parse(toml_str).unwrap();
    assert_eq!(cfg.proposer.model_override, None);
}

#[test]
fn model_override_set_value_is_some() {
    let toml_str = r#"
name = "test"

[clone]
expected_root = "~/work/repo*"

[proposer]
model_override = "claude-opus-4-7"
"#;
    let cfg = config::parse(toml_str).unwrap();
    assert_eq!(cfg.proposer.model_override, Some("claude-opus-4-7".into()));
}

// --------------------------------------------------------------------------
// 10. Bundled agent bootstrap: files get written when absent, not overwritten.
// --------------------------------------------------------------------------
#[test]
fn bootstrap_writes_three_agents() {
    let dir = tempfile::tempdir().unwrap();
    let written = pi_coding_agent::halo::bootstrap_bundled_agents(dir.path()).unwrap();
    assert_eq!(written.len(), 3, "should write exactly 3 agent files");
    // Check that files exist.
    for p in &written {
        assert!(p.is_file(), "agent file should exist: {:?}", p);
        let content = std::fs::read_to_string(p).unwrap();
        assert!(!content.is_empty(), "agent file should not be empty: {:?}", p);
    }
}

#[test]
fn bootstrap_does_not_overwrite_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path().join(".pi").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    // Pre-create one agent with custom content.
    let custom = "# My custom proposer\n";
    std::fs::write(agents_dir.join("halo-proposer.md"), custom).unwrap();

    let written = pi_coding_agent::halo::bootstrap_bundled_agents(dir.path()).unwrap();
    // Only 2 files should be written (implementer + reviewer).
    assert_eq!(written.len(), 2, "should only write missing agents");

    // Custom file should be unchanged.
    let content = std::fs::read_to_string(agents_dir.join("halo-proposer.md")).unwrap();
    assert_eq!(content, custom);
}

// --------------------------------------------------------------------------
// 11. --halo-status empty-state output.
// --------------------------------------------------------------------------
#[test]
fn halo_status_empty_state_says_not_running() {
    // We exercise the status rendering via the render_status helper.
    // Since the test runs in a tmp dir with no halo state, we expect SUPERVISOR_NOT_RUNNING.
    let dir = tempfile::tempdir().unwrap();
    let status = pi_coding_agent::halo::render_status(dir.path()).unwrap();
    assert!(
        status.contains("SUPERVISOR_NOT_RUNNING"),
        "expected SUPERVISOR_NOT_RUNNING, got: {}",
        status
    );
}

// --------------------------------------------------------------------------
// 12. Halo-owned clone preconditions.
// --------------------------------------------------------------------------
#[test]
fn clone_precondition_rejects_missing_expected_root() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config::Config {
        name: "test".into(),
        target_branch: "main".into(),
        clone_config: config::CloneConfig { expected_root: None },
        guardrails: Default::default(),
        supervisor: Default::default(),
        smoke: Default::default(),
        proposer: Default::default(),
        cycle: Default::default(),
        orchestrate: Default::default(),
    };
    let err = pi_coding_agent::halo::check_halo_clone_preconditions(dir.path(), &cfg)
        .unwrap_err();
    assert!(
        err.to_string().contains("clone.expected_root not set"),
        "expected 'not set' error, got: {}",
        err
    );
}

#[test]
fn clone_precondition_rejects_non_matching_path() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config::Config {
        name: "test".into(),
        target_branch: "main".into(),
        clone_config: config::CloneConfig {
            expected_root: Some("/this/path/does/not/match/*".into()),
        },
        guardrails: Default::default(),
        supervisor: Default::default(),
        smoke: Default::default(),
        proposer: Default::default(),
        cycle: Default::default(),
        orchestrate: Default::default(),
    };
    let err = pi_coding_agent::halo::check_halo_clone_preconditions(dir.path(), &cfg)
        .unwrap_err();
    assert!(
        err.to_string().contains("does not match expected_root"),
        "expected glob mismatch error, got: {}",
        err
    );
}
