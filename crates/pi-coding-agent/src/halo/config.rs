//! `halo.toml` schema — RFD 0025 v0.28 §Config.
//!
//! `#[serde(deny_unknown_fields)]` on every struct (mirrors RFD 0021).

use serde::{Deserialize, Serialize};
use anyhow::Result;

// ── Top-level ────────────────────────────────────────────────────────────────

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
}

fn default_target_branch() -> String { "halo/auto-merge".into() }

// ── [clone] ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneConfig {
    pub expected_root: Option<String>,
}

// ── [guardrails] ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guardrails {
    /// v0.27: renamed from daily_cost_cap_usd.
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

// ── [supervisor] ─────────────────────────────────────────────────────────────

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

// ── [smoke] ──────────────────────────────────────────────────────────────────

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

// ── [proposer] ───────────────────────────────────────────────────────────────

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

// ── [cycle] ──────────────────────────────────────────────────────────────────

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

// ── [orchestrate] ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orchestrate {
    #[serde(default = "d_auto_approve")]
    pub auto_approve: String,
    #[serde(default = "d_reviewer_agent")]
    pub reviewer_agent: String,
    /// v0.27: renamed from per_cycle_cost_cap_usd.
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

// ── Parse + Validate ──────────────────────────────────────────────────────────

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

    errs
}
