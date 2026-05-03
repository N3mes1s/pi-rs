//! Per RFD 0028 §A.8 test plan.

use pi_build::{parse, validate, Manifest, ManifestError, ProviderName, ThinkingLevel};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(rel: &str) -> String {
    let path = format!("{FIXTURES}/{rel}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

// --- Round-trip ---

#[test]
fn round_trip_dice_oracle() {
    let raw = fixture("valid/dice-oracle.toml");
    let m = parse(&raw).expect("parse dice-oracle");
    let serialized = toml::to_string(&m).expect("serialize");
    let m2 = parse(&serialized).expect("re-parse");
    assert_eq!(m, m2, "round-trip equality");
}

// --- Schema version routing ---

#[test]
fn schema_version_2_returns_too_new_not_unknown_field() {
    let raw = r#"
schema_version = 2
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).expect_err("v2 should fail");
    match err {
        ManifestError::SchemaTooNew { found, supported } => {
            assert_eq!(found, 2);
            assert_eq!(supported, 1);
        }
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }
}

#[test]
fn schema_version_0_returns_too_old() {
    let raw = "schema_version = 0";
    let err = parse(raw).expect_err("v0 should fail");
    assert!(matches!(err, ManifestError::SchemaTooOld { found: 0 }), "{err:?}");
}

#[test]
fn schema_version_negative_fails_at_version_detect() {
    let raw = "schema_version = -1";
    let err = parse(raw).expect_err("negative version should fail");
    assert!(matches!(err, ManifestError::VersionDetect(_)), "{err:?}");
}

#[test]
fn empty_file_fails_at_version_detect() {
    let err = parse("").expect_err("empty file should fail");
    assert!(matches!(err, ManifestError::VersionDetect(_)), "{err:?}");
}

// --- Defaults applied ---

#[test]
fn minimal_defaults_parse_with_pi_sdk_defaults() {
    let raw = fixture("valid/minimal-defaults.toml");
    let m = parse(&raw).expect("parse minimal");
    assert!(m.secrets.required.is_empty());
    assert_eq!(
        m.tools.allowlist,
        vec!["read".to_string(), "grep".into(), "find".into(), "ls".into()],
    );
    assert_eq!(m.runtime.max_session_tokens, 10_000_000);
    assert_eq!(m.runtime.max_tool_invocations_per_turn, 64);
    assert_eq!(m.runtime.max_recursion, 8);
    assert_eq!(m.provider.name, ProviderName::Anthropic);
    assert_eq!(m.provider.thinking, ThinkingLevel::Off);
}

// --- disallow_unsafe enforcement ---

#[test]
fn disallow_unsafe_rejects_bash() {
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
tools = { allowlist = ["bash"], disallow_unsafe = true }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).expect_err("disallow_unsafe + bash should fail");
    assert!(
        matches!(err, ManifestError::UnsafeToolWithDisallow(ref s) if s == "bash"),
        "{err:?}",
    );
}

// --- Tool name case sensitivity ---

#[test]
fn tool_name_case_sensitive_lowercase_only() {
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
tools = { allowlist = ["Read"] }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).expect_err("`Read` should be rejected");
    assert!(
        matches!(err, ManifestError::UnknownTool(ref s) if s == "Read"),
        "{err:?}",
    );
}

// --- Allowlist dedup ---

#[test]
fn allowlist_dedup_silent_preserves_first_occurrence() {
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
tools = { allowlist = ["read", "grep", "read", "find", "grep"] }
runtime = { system_prompt = "p" }
"#;
    let m = parse(raw).expect("dedup is silent");
    assert_eq!(
        m.tools.allowlist,
        vec!["read".to_string(), "grep".into(), "find".into()],
    );
}

#[test]
fn empty_allowlist_after_dedup_fails() {
    // Empty array — no items to dedup, but the EmptyAllowlist
    // check fires. (User wrote `allowlist = []` explicitly.)
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
tools = { allowlist = [] }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).expect_err("empty allowlist should fail");
    assert!(matches!(err, ManifestError::EmptyAllowlist), "{err:?}");
}

// --- Length boundaries ---

#[test]
fn description_at_boundary_1024_passes_1025_fails() {
    let make = |dlen: usize| {
        let desc = "a".repeat(dlen);
        format!(
            r#"schema_version = 1
agent = {{ name = "x", description = "{desc}", version = "0.1.0" }}
provider = {{ name = "anthropic", model = "m" }}
runtime = {{ system_prompt = "p" }}
"#
        )
    };
    parse(&make(1024)).expect("1024 bytes should pass");
    let err = parse(&make(1025)).expect_err("1025 bytes should fail");
    assert!(
        matches!(err, ManifestError::InvalidDescription { len: 1025 }),
        "{err:?}",
    );
}

#[test]
fn system_prompt_at_boundary_65536_passes_65537_fails() {
    let make = |slen: usize| {
        let prompt = "a".repeat(slen);
        format!(
            r#"schema_version = 1
agent = {{ name = "x", description = "y", version = "0.1.0" }}
provider = {{ name = "anthropic", model = "m" }}
runtime = {{ system_prompt = "{prompt}" }}
"#
        )
    };
    parse(&make(65_536)).expect("65_536 bytes should pass");
    let err = parse(&make(65_537)).expect_err("65_537 bytes should fail");
    assert!(
        matches!(err, ManifestError::InvalidSystemPromptLen { len: 65_537 }),
        "{err:?}",
    );
}

#[test]
fn model_at_boundary_256_passes_257_fails() {
    let make = |mlen: usize| {
        let model = "m".repeat(mlen);
        format!(
            r#"schema_version = 1
agent = {{ name = "x", description = "y", version = "0.1.0" }}
provider = {{ name = "anthropic", model = "{model}" }}
runtime = {{ system_prompt = "p" }}
"#
        )
    };
    parse(&make(256)).expect("256 bytes should pass");
    let err = parse(&make(257)).expect_err("257 bytes should fail");
    assert!(
        matches!(err, ManifestError::InvalidModelLen { len: 257 }),
        "{err:?}",
    );
}

// --- max_recursion boundaries ---

#[test]
fn max_recursion_boundaries() {
    let make = |n: u64| {
        format!(
            r#"schema_version = 1
agent = {{ name = "x", description = "y", version = "0.1.0" }}
provider = {{ name = "anthropic", model = "m" }}
runtime = {{ system_prompt = "p", max_recursion = {n} }}
"#
        )
    };
    // Accept: 1, 8, 16
    parse(&make(1)).expect("1 should pass");
    parse(&make(8)).expect("8 should pass");
    parse(&make(16)).expect("16 should pass");
    // Reject: 0, 17
    assert!(matches!(
        parse(&make(0)).unwrap_err(),
        ManifestError::MaxRecursionOutOfRange { found: 0 }
    ));
    assert!(matches!(
        parse(&make(17)).unwrap_err(),
        ManifestError::MaxRecursionOutOfRange { found: 17 }
    ));
}

// --- Per-error coverage sweep ---

#[test]
fn invalid_agent_name_uppercase_rejected() {
    let raw = r#"
schema_version = 1
agent = { name = "Bad", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).unwrap_err();
    assert!(matches!(err, ManifestError::InvalidAgentName(ref s) if s == "Bad"), "{err:?}");
}

#[test]
fn invalid_version_not_semver_rejected() {
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "not-semver" }
provider = { name = "anthropic", model = "m" }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).unwrap_err();
    assert!(
        matches!(err, ManifestError::InvalidVersion(ref s, _) if s == "not-semver"),
        "{err:?}",
    );
}

#[test]
fn invalid_env_var_name_lowercase_rejected() {
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
secrets = { required = ["lowercase_key"] }
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).unwrap_err();
    assert!(
        matches!(err, ManifestError::InvalidEnvVarName(ref s) if s == "lowercase_key"),
        "{err:?}",
    );
}

#[test]
fn max_session_tokens_below_floor_rejected() {
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
runtime = { system_prompt = "p", max_session_tokens = 999 }
"#;
    let err = parse(raw).unwrap_err();
    assert!(
        matches!(err, ManifestError::MaxSessionTokensTooLow { found: 999 }),
        "{err:?}",
    );
}

#[test]
fn unknown_field_under_tools_table_rejected() {
    // The `[tools.bash]` reserved syntax → `ManifestError::Parse`
    // with field `bash` under ToolsConfig (per A.9 wording).
    let raw = r#"
schema_version = 1
agent = { name = "x", description = "y", version = "0.1.0" }
provider = { name = "anthropic", model = "m" }
[tools]
allowlist = ["read"]
[tools.bash]
timeout_ms = 30_000
runtime = { system_prompt = "p" }
"#;
    let err = parse(raw).unwrap_err();
    assert!(matches!(err, ManifestError::Parse(_)), "{err:?}");
    let msg = format!("{err}");
    assert!(
        msg.contains("bash"),
        "error message should name the unknown field 'bash': {msg}",
    );
}

// --- Direct validate() entry point (exercise the public surface) ---

#[test]
fn validate_runs_on_already_parsed_manifest() {
    let raw = fixture("valid/dice-oracle.toml");
    let mut m: Manifest = toml::from_str(&raw).expect("toml parse");
    validate(&mut m).expect("validate should pass on dice-oracle");
}
