//! `halo.toml` schema — RFD 0025 v0.28 §Config.
//!
//! `#[serde(deny_unknown_fields)]` on every struct (mirrors RFD 0021).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    #[serde(default = "default_target_branch")]
    pub target_branch: String,
    #[serde(default, rename = "clone")]
    pub clone_config: CloneConfig,
    #[serde(default, rename = "guardrails")]
    pub guardrails: Guardrails,
    #[serde(default, rename = "supervisor")]
    pub supervisor: Supervisor,
    #[serde(default, rename = "smoke")]
    pub smoke: Smoke,
    #[serde(default, rename = "proposer")]
    pub proposer: Proposer,
    #[serde(default, rename = "cycle")]
    pub cycle: Cycle,
    #[serde(default, rename = "orchestrate")]
    pub orchestrate: Orchestrate,
    /// Per RFD 0028 §D.2: array of compiled-agent cycle declarations.
    /// Empty or absent → halo runs only the existing orchestrate
    /// cycle shape (pre-RFD-0028 behavior, unchanged). Non-empty →
    /// halo runs each declared compiled agent as a sibling cycle
    /// shape; the cycle-driver dispatch wiring lands in a follow-up
    /// commit (Phase 2 of Commit D).
    ///
    /// Note: spec v0.10 originally proposed `[[cycle]]` for this
    /// array, but `[cycle]` already exists as a singular table —
    /// TOML disallows the same key being both. v0.14 renamed to
    /// `[[compiled_agent]]`.
    #[serde(default, rename = "compiled_agent")]
    pub compiled_agents: Vec<CompiledAgentSpec>,
}

fn default_target_branch() -> String { "halo/auto-merge".into() }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneConfig {
    pub expected_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guardrails {
    #[serde(default = "d_daily_budget")]
    pub daily_spend_budget_usd: f64,
    #[serde(default = "d_commits_per_hour")]
    pub commits_per_hour_max: u32,
    #[serde(default = "d_streak_max")]
    pub failed_build_streak_max: u32,
    #[serde(default = "d_min_cycle_secs")]
    pub min_seconds_between_cycles: u64,
    #[serde(default)]
    pub quiet_hours_utc: String,
    #[serde(default = "d_cycles_per_day")]
    pub cycles_per_day_max: u32,
}

fn d_daily_budget() -> f64 { 10.0 }
fn d_commits_per_hour() -> u32 { 4 }
fn d_streak_max() -> u32 { 2 }
fn d_min_cycle_secs() -> u64 { 1800 }
fn d_cycles_per_day() -> u32 { 24 }

impl Default for Guardrails {
    fn default() -> Self {
        Self {
            daily_spend_budget_usd: d_daily_budget(),
            commits_per_hour_max: d_commits_per_hour(),
            failed_build_streak_max: d_streak_max(),
            min_seconds_between_cycles: d_min_cycle_secs(),
            quiet_hours_utc: String::new(),
            cycles_per_day_max: d_cycles_per_day(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supervisor {
    #[serde(default = "d_grace_secs")]
    pub interrupt_grace_seconds: u64,
}

fn d_grace_secs() -> u64 { 30 }

impl Default for Supervisor {
    fn default() -> Self { Self { interrupt_grace_seconds: d_grace_secs() } }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Smoke {
    #[serde(default = "d_smoke_cmd")]
    pub cmd: String,
    #[serde(default = "d_smoke_timeout")]
    pub timeout_seconds: u64,
}

fn d_smoke_cmd() -> String {
    "cargo build --workspace --target x86_64-unknown-linux-musl".into()
}
fn d_smoke_timeout() -> u64 { 1200 }

impl Default for Smoke {
    fn default() -> Self {
        Self { cmd: d_smoke_cmd(), timeout_seconds: d_smoke_timeout() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposer {
    #[serde(default = "d_proposer_agent")]
    pub agent: String,
    #[serde(default, deserialize_with = "deser_opt_empty_str")]
    pub model_override: Option<String>,
    #[serde(default = "d_max_retries")]
    pub max_retries: u32,
    #[serde(default = "d_proposals_per_refill")]
    pub proposals_per_refill: u32,
    #[serde(default = "d_refill_threshold")]
    pub refill_threshold: u32,
    #[serde(default = "d_proposer_cost")]
    pub estimated_cost_usd_per_call: f64,
    #[serde(default = "d_cooldown_hours")]
    pub proposal_retry_cooldown_hours: f64,
    #[serde(default = "d_min_proposer_secs")]
    pub min_seconds_between_proposer_runs: u64,
}

fn d_proposer_agent() -> String { "halo-proposer".into() }
fn d_max_retries() -> u32 { 3 }
fn d_proposals_per_refill() -> u32 { 5 }
fn d_refill_threshold() -> u32 { 3 }
fn d_proposer_cost() -> f64 { 0.30 }
fn d_cooldown_hours() -> f64 { 48.0 }
fn d_min_proposer_secs() -> u64 { 7200 }

fn deser_opt_empty_str<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where D: serde::Deserializer<'de> {
    let s: Option<String> = serde::Deserialize::deserialize(d)?;
    Ok(s.filter(|v| !v.is_empty()))
}

impl Default for Proposer {
    fn default() -> Self {
        Self {
            agent: d_proposer_agent(),
            model_override: None,
            max_retries: d_max_retries(),
            proposals_per_refill: d_proposals_per_refill(),
            refill_threshold: d_refill_threshold(),
            estimated_cost_usd_per_call: d_proposer_cost(),
            proposal_retry_cooldown_hours: d_cooldown_hours(),
            min_seconds_between_proposer_runs: d_min_proposer_secs(),
        }
    }
}

/// Canonical eight-step list, in required order.
pub const CANONICAL_STEPS: &[&str] = &[
    "pick_proposal", "synthesise_campaign", "prep_branch", "orchestrate",
    "keep_marker_scan", "smoke", "rollback_if_regress", "evolve_tick",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cycle {
    #[serde(default = "d_steps")]
    pub steps: Vec<String>,
    #[serde(default = "d_keep_branches")]
    pub keep_branches: u32,
}

fn d_steps() -> Vec<String> { CANONICAL_STEPS.iter().map(|s| s.to_string()).collect() }
fn d_keep_branches() -> u32 { 50 }

impl Default for Cycle {
    fn default() -> Self { Self { steps: d_steps(), keep_branches: d_keep_branches() } }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orchestrate {
    #[serde(default = "d_auto_approve")]
    pub auto_approve: String,
    #[serde(default = "d_reviewer_agent")]
    pub reviewer_agent: String,
    #[serde(default = "d_per_cycle_threshold")]
    pub per_cycle_overspend_threshold_usd: f64,
    #[serde(default = "d_budget_per_min")]
    pub budget_dollars_per_minute_estimate: f64,
}

fn d_auto_approve() -> String { "auto-policy".into() }
fn d_reviewer_agent() -> String { "code-reviewer".into() }
fn d_per_cycle_threshold() -> f64 { 4.0 }
fn d_budget_per_min() -> f64 { 0.20 }

impl Default for Orchestrate {
    fn default() -> Self {
        Self {
            auto_approve: d_auto_approve(),
            reviewer_agent: d_reviewer_agent(),
            per_cycle_overspend_threshold_usd: d_per_cycle_threshold(),
            budget_dollars_per_minute_estimate: d_budget_per_min(),
        }
    }
}

/// Per RFD 0028 §D.2 — one compiled-agent cycle declaration in
/// halo.toml. The cycle-driver runs each as a separate cycle
/// shape (alongside the existing orchestrate cycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledAgentSpec {
    /// Display name for the cycle log + alerts. Must be unique
    /// across all `[[compiled_agent]]` blocks in this halo.toml.
    pub name: String,

    /// Path to the compiled agent binary. Resolution per §D.2:
    /// - starts with "/" → absolute path used verbatim.
    /// - contains "/"    → relative to halo.toml's parent dir.
    /// - no "/"          → resolved via `$PATH`.
    pub binary: String,

    /// Args appended after the binary. v1 ALWAYS forces `--jsonl`
    /// for spend attribution, regardless of whether the operator
    /// listed it here (Commit B's arg parser is `any(|a| a ==
    /// "--jsonl")` so duplicates are harmless).
    #[serde(default)]
    pub args: Vec<String>,

    /// Piped to the binary's stdin. Compiled agents read this via
    /// `read_prompt_from_args_or_stdin` per RFD 0028 §B.11.
    pub prompt: String,

    /// Exit-code → policy table per §D.6. Required: a row for 0.
    /// Unspecified codes default to `Alert` at runtime. The optional
    /// `"*"` wildcard catches any unspecified code; without it,
    /// "alert" is the safe-by-default choice.
    pub on_exit: BTreeMap<String, ExitPolicy>,

    /// Wall-clock cap for the cycle. Default 3600 (1 hour) — matches
    /// halo's safety-by-default ethos elsewhere. `0` = explicit
    /// no-cap (not recommended).
    #[serde(default = "d_timeout_secs")]
    pub timeout_secs: u64,

    /// Additional env vars to set on the child process beyond what
    /// halo inherits from its OWN env. NOT the secrets channel —
    /// halo's env IS that. This is for cycle-specific tags
    /// (e.g., `CYCLE_NAME = "fix-flaky-tests"`).
    #[serde(default)]
    pub env_extra: BTreeMap<String, String>,

    /// Per §D.6 throttle math: `min(2^streak * base_delay_secs,
    /// cap_secs)`. After `streak_max` consecutive throttle
    /// outcomes, halo pauses entirely.
    #[serde(default = "d_throttle_streak_max")]
    pub throttle_streak_max: u32,
    #[serde(default = "d_throttle_base_delay_secs")]
    pub throttle_base_delay_secs: u64,
    #[serde(default = "d_throttle_cap_secs")]
    pub throttle_cap_secs: u64,
}

fn d_timeout_secs() -> u64 { 3600 }
fn d_throttle_streak_max() -> u32 { 5 }
fn d_throttle_base_delay_secs() -> u64 { 60 }
fn d_throttle_cap_secs() -> u64 { 3600 }

impl CompiledAgentSpec {
    /// Resolve `self.binary` against the halo.toml's parent directory
    /// per RFD 0028 §D.2 path-resolution rules:
    /// - absolute (`/usr/local/bin/agent`) → used verbatim.
    /// - contains `/` (`./bin/agent`) → relative to `halo_toml_parent`.
    /// - bare name (`agent`) → returned as-is for `Command::new` to
    ///   resolve via `$PATH`.
    ///
    /// Phase 2b's wiring code (cycle-driver loop in `run.rs`) calls
    /// this before constructing `CycleSubprocessCommand`. Per
    /// rfd-critic post-Phase-1 review: subprocess.rs's spawn doc
    /// claimed config.rs's parser canonicalised these paths, but
    /// no code did — the comment described an intent, not behavior.
    /// This method makes the intent real.
    pub fn resolve_binary(&self, halo_toml_parent: &Path) -> PathBuf {
        let p = Path::new(&self.binary);
        if p.is_absolute() {
            p.to_path_buf()
        } else if self.binary.contains('/') {
            halo_toml_parent.join(p)
        } else {
            // Bare name → `Command::new` does $PATH resolution.
            p.to_path_buf()
        }
    }

    /// Lower `timeout_secs` to the `Option<Duration>` shape
    /// `spawn_cycle_subprocess` expects. `0` is the operator's
    /// EXPLICIT no-cap (per RFD §D.2); any non-zero value
    /// becomes `Some(Duration::from_secs(n))`.
    ///
    /// Without this helper, Phase 2b wiring could naively pass
    /// `Some(Duration::from_secs(0))`, which would expire the
    /// cycle immediately on the first poll.
    pub fn timeout(&self) -> Option<Duration> {
        if self.timeout_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.timeout_secs))
        }
    }
}

/// Per RFD 0028 §D.6 exit-code → behavior mapping. The serde
/// `rename_all = "snake_case"` gives the operator the
/// "continue"/"alert"/"throttle" string spelling on the wire.
/// Future variants (Pause, Restart, ...) land additively at v2
/// without a string-parsing migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitPolicy {
    Continue,
    Alert,
    Throttle,
}

/// Parse a TOML string into a [`Config`].
pub fn parse(toml_str: &str) -> Result<Config> {
    toml::from_str(toml_str).map_err(|e| anyhow::anyhow!("halo.toml parse error: {}", e))
}

/// Validate a parsed config. Returns a list of error strings.
/// `allow_main` corresponds to the `--halo-allow-main` CLI flag.
pub fn validate(cfg: &Config, allow_main: bool) -> Vec<String> {
    let mut errs = Vec::new();

    if cfg.target_branch == "main" && !allow_main {
        errs.push(
            "target_branch = \"main\" is not allowed without --halo-allow-main. \
             Use target_branch = \"halo/auto-merge\" (the recommended default)."
                .into(),
        );
    }
    if cfg.orchestrate.auto_approve == "yolo" {
        errs.push(
            "orchestrate.auto_approve = \"yolo\" is not allowed. \
             Use \"auto-policy\" or \"auto-judge\"."
                .into(),
        );
    }

    let mut check = |label: &str, v: f64| {
        if v < 0.0 || v.is_nan() {
            errs.push(format!("{label} must be a non-negative finite number, got {v}"));
        }
    };
    check("guardrails.daily_spend_budget_usd", cfg.guardrails.daily_spend_budget_usd);
    check("orchestrate.per_cycle_overspend_threshold_usd", cfg.orchestrate.per_cycle_overspend_threshold_usd);
    check("orchestrate.budget_dollars_per_minute_estimate", cfg.orchestrate.budget_dollars_per_minute_estimate);
    check("proposer.estimated_cost_usd_per_call", cfg.proposer.estimated_cost_usd_per_call);

    let canonical: Vec<String> = CANONICAL_STEPS.iter().map(|s| s.to_string()).collect();
    if cfg.cycle.steps != canonical {
        errs.push(format!(
            "[cycle].steps does not match the canonical eight-step list. \
             Expected (in order): {CANONICAL_STEPS:?}. \
             See §Tree hygiene + cycle ordering in RFD 0025."
        ));
    }

    match cfg.clone_config.expected_root.as_ref() {
        None => errs.push("clone.expected_root not set".into()),
        Some(s) if s.trim().is_empty() => errs.push("clone.expected_root not set".into()),
        Some(_) => {}
    }

    // Compiled-agent specs (RFD 0028 §D.2):
    //   - name unique across all blocks
    //   - non-empty name + binary + prompt
    //   - on_exit has a row for "0" (Required per §D.6)
    //   - on_exit keys are either "*" or parseable as i32
    let mut seen_names = std::collections::HashSet::new();
    for spec in &cfg.compiled_agents {
        if spec.name.trim().is_empty() {
            errs.push("[[compiled_agent]] name is empty".into());
        } else if !seen_names.insert(spec.name.clone()) {
            errs.push(format!(
                "[[compiled_agent]] name {:?} declared more than once",
                spec.name
            ));
        }
        if spec.binary.trim().is_empty() {
            errs.push(format!(
                "[[compiled_agent]] {:?}: binary is empty",
                spec.name
            ));
        }
        if spec.prompt.trim().is_empty() {
            errs.push(format!(
                "[[compiled_agent]] {:?}: prompt is empty",
                spec.name
            ));
        }
        if !spec.on_exit.contains_key("0") {
            errs.push(format!(
                "[[compiled_agent]] {:?}: on_exit MUST declare a policy for exit code 0",
                spec.name
            ));
        }
        for key in spec.on_exit.keys() {
            if key == "*" {
                continue;
            }
            if key.parse::<i32>().is_err() {
                errs.push(format!(
                    "[[compiled_agent]] {:?}: on_exit key {:?} is not a valid exit code (use a number or \"*\")",
                    spec.name, key
                ));
            }
        }

        // Throttle-field range checks (post-Phase-2c reviewer):
        // - streak_max = 0 would pause halo on the FIRST throttle
        //   (since streak_max=0 trips the `new_streak >= max` check
        //   on the very first throttle event). Operators almost
        //   certainly mean "never pause" with 0; reject + ask them
        //   to set a real number.
        // - base_delay_secs = 0 produces a 0-duration backoff —
        //   no rate-limit between throttle events. Reject.
        // - cap_secs < base_delay_secs makes the math nonsensical
        //   (cap can't be tighter than the very first delay).
        if spec.throttle_streak_max == 0 {
            errs.push(format!(
                "[[compiled_agent]] {:?}: throttle_streak_max must be > 0; \
                 a value of 0 would pause halo on the first throttle event",
                spec.name
            ));
        }
        if spec.throttle_base_delay_secs == 0 {
            errs.push(format!(
                "[[compiled_agent]] {:?}: throttle_base_delay_secs must be > 0 \
                 (otherwise throttle backoff has no effect)",
                spec.name
            ));
        }
        if spec.throttle_cap_secs < spec.throttle_base_delay_secs {
            errs.push(format!(
                "[[compiled_agent]] {:?}: throttle_cap_secs ({}) must be >= throttle_base_delay_secs ({})",
                spec.name, spec.throttle_cap_secs, spec.throttle_base_delay_secs
            ));
        }
    }

    errs
}

#[cfg(test)]
mod compiled_agent_tests {
    //! Per RFD 0028 §D.2 schema tests. Phase 1 of Commit D ships
    //! the parsing + validation; Phase 2 wires the cycle-driver
    //! dispatch. Until then these specs are PARSED but not
    //! EXECUTED — operators can pre-author halo.toml against
    //! the schema today.

    use super::*;

    fn base_cfg() -> &'static str {
        r#"name = "halo-test"
target_branch = "halo/auto-merge"
[clone]
expected_root = "/work/halo-clone"
"#
    }

    #[test]
    fn no_compiled_agents_parses_clean() {
        let cfg = parse(base_cfg()).expect("base parses");
        assert!(cfg.compiled_agents.is_empty());
    }

    #[test]
    fn one_compiled_agent_with_defaults_parses_and_validates() {
        let toml = format!(
            "{base}\n[[compiled_agent]]\nname = \"fix-flaky\"\nbinary = \"./bin/agent\"\nprompt = \"audit failures\"\non_exit = {{ \"0\" = \"continue\", \"1\" = \"alert\", \"3\" = \"throttle\" }}\n",
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        assert_eq!(cfg.compiled_agents.len(), 1);
        let spec = &cfg.compiled_agents[0];
        assert_eq!(spec.name, "fix-flaky");
        assert_eq!(spec.binary, "./bin/agent");
        assert_eq!(spec.timeout_secs, 3600);
        assert_eq!(spec.throttle_streak_max, 5);
        assert_eq!(spec.throttle_base_delay_secs, 60);
        assert_eq!(spec.throttle_cap_secs, 3600);
        assert!(spec.args.is_empty());
        assert!(spec.env_extra.is_empty());
        assert_eq!(spec.on_exit.get("0"), Some(&ExitPolicy::Continue));
        assert_eq!(spec.on_exit.get("1"), Some(&ExitPolicy::Alert));
        assert_eq!(spec.on_exit.get("3"), Some(&ExitPolicy::Throttle));
        let errs = validate(&cfg, false);
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn env_extra_and_args_round_trip() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "with-env"
binary  = "./bin/agent"
args    = ["--jsonl", "--debug"]
prompt  = "..."
on_exit = {{ "0" = "continue" }}
[compiled_agent.env_extra]
CYCLE_NAME = "with-env"
GIT_PAGER  = "cat"
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let spec = &cfg.compiled_agents[0];
        assert_eq!(spec.args, vec!["--jsonl".to_string(), "--debug".into()]);
        assert_eq!(spec.env_extra.get("CYCLE_NAME"), Some(&"with-env".to_string()));
        assert_eq!(spec.env_extra.get("GIT_PAGER"), Some(&"cat".to_string()));
    }

    #[test]
    fn duplicate_name_caught_at_validate() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "dupe"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue" }}

[[compiled_agent]]
name    = "dupe"
binary  = "/y"
prompt  = "q"
on_exit = {{ "0" = "continue" }}
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(
            errs.iter().any(|e| e.contains("declared more than once")),
            "{errs:?}",
        );
    }

    #[test]
    fn missing_zero_row_caught() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "no-zero"
binary  = "/x"
prompt  = "p"
on_exit = {{ "1" = "alert" }}
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(
            errs.iter().any(|e| e.contains("MUST declare a policy for exit code 0")),
            "{errs:?}",
        );
    }

    #[test]
    fn wildcard_key_accepted() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "wild"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue", "*" = "alert" }}
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn non_numeric_non_wildcard_key_caught() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "bad-key"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue", "abc" = "alert" }}
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(
            errs.iter().any(|e| e.contains("not a valid exit code")),
            "{errs:?}",
        );
    }

    // ── Adversarial-review-2 helper methods ──────────────────────

    #[test]
    fn resolve_binary_absolute_path_used_verbatim() {
        let spec = CompiledAgentSpec {
            name: "x".into(),
            binary: "/usr/local/bin/agent".into(),
            args: vec![],
            prompt: "p".into(),
            on_exit: BTreeMap::new(),
            timeout_secs: 0,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 0,
            throttle_base_delay_secs: 0,
            throttle_cap_secs: 0,
        };
        let resolved = spec.resolve_binary(Path::new("/halo/parent"));
        assert_eq!(resolved, PathBuf::from("/usr/local/bin/agent"));
    }

    #[test]
    fn resolve_binary_relative_path_joins_halo_parent() {
        let spec = CompiledAgentSpec {
            name: "x".into(),
            binary: "./bin/agent".into(),
            args: vec![],
            prompt: "p".into(),
            on_exit: BTreeMap::new(),
            timeout_secs: 0,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 0,
            throttle_base_delay_secs: 0,
            throttle_cap_secs: 0,
        };
        let resolved = spec.resolve_binary(Path::new("/work/.pi"));
        assert_eq!(resolved, PathBuf::from("/work/.pi/./bin/agent"));
    }

    #[test]
    fn resolve_binary_bare_name_returned_as_is_for_path_lookup() {
        let spec = CompiledAgentSpec {
            name: "x".into(),
            binary: "agent".into(),
            args: vec![],
            prompt: "p".into(),
            on_exit: BTreeMap::new(),
            timeout_secs: 0,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 0,
            throttle_base_delay_secs: 0,
            throttle_cap_secs: 0,
        };
        // Bare name → returned unchanged; Command::new resolves via $PATH.
        assert_eq!(spec.resolve_binary(Path::new("/anywhere")), PathBuf::from("agent"));
    }

    #[test]
    fn timeout_zero_lowers_to_none_per_rfd_d2() {
        let spec = CompiledAgentSpec {
            name: "x".into(),
            binary: "/x".into(),
            args: vec![],
            prompt: "p".into(),
            on_exit: BTreeMap::new(),
            timeout_secs: 0,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 0,
            throttle_base_delay_secs: 0,
            throttle_cap_secs: 0,
        };
        assert_eq!(spec.timeout(), None);
    }

    #[test]
    fn timeout_non_zero_lowers_to_some_duration() {
        let mut spec = CompiledAgentSpec {
            name: "x".into(),
            binary: "/x".into(),
            args: vec![],
            prompt: "p".into(),
            on_exit: BTreeMap::new(),
            timeout_secs: 1800,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 0,
            throttle_base_delay_secs: 0,
            throttle_cap_secs: 0,
        };
        assert_eq!(spec.timeout(), Some(Duration::from_secs(1800)));
        spec.timeout_secs = 1;
        assert_eq!(spec.timeout(), Some(Duration::from_secs(1)));
    }

    // ── Throttle-field range checks (post-Phase-2c reviewer) ─────

    #[test]
    fn throttle_streak_max_zero_rejected_at_validate() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "x"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue" }}
throttle_streak_max = 0
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(
            errs.iter().any(|e| e.contains("throttle_streak_max must be > 0")),
            "{errs:?}",
        );
    }

    #[test]
    fn throttle_base_delay_zero_rejected_at_validate() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "x"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue" }}
throttle_base_delay_secs = 0
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(
            errs.iter().any(|e| e.contains("throttle_base_delay_secs must be > 0")),
            "{errs:?}",
        );
    }

    #[test]
    fn throttle_cap_below_base_rejected_at_validate() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "x"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue" }}
throttle_base_delay_secs = 60
throttle_cap_secs        = 30
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(
            errs.iter().any(|e| e.contains("throttle_cap_secs")),
            "{errs:?}",
        );
    }

    #[test]
    fn throttle_defaults_pass_validate() {
        // Defaults: streak_max=5, base=60, cap=3600 — all sane.
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "default-throttle"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "continue" }}
"#,
            base = base_cfg()
        );
        let cfg = parse(&toml).expect("parses");
        let errs = validate(&cfg, false);
        assert!(errs.is_empty(), "default throttle config should validate cleanly: {errs:?}");
    }

    #[test]
    fn unknown_exit_policy_string_rejected_at_serde() {
        let toml = format!(
            r#"{base}
[[compiled_agent]]
name    = "x"
binary  = "/x"
prompt  = "p"
on_exit = {{ "0" = "yolo" }}
"#,
            base = base_cfg()
        );
        let err = parse(&toml).expect_err("yolo is not a valid ExitPolicy");
        let msg = format!("{err}");
        assert!(msg.contains("yolo") || msg.contains("variant"), "{msg}");
    }
}
