//! Per RFD 0028 §D — compiled-agent cycle driver.
//!
//! Phase 2b.i scope (this commit): pure library function
//! `run_compiled_agent_cycle` that executes one declared
//! `CompiledAgentSpec` end-to-end. Self-contained — does NOT
//! touch the existing 1.4 KLoC `cycle.rs` critical path.
//!
//! Phase 2c (separate authorization): wire this into the
//! `run.rs` cycle-driver loop so halo automatically dispatches
//! compiled-agent cycles per the operator's halo.toml
//! `[[compiled_agent]]` blocks.
//!
//! The library shape:
//! - Inputs are passed explicitly (no `CycleCtx` dependency)
//!   so the function can be called from a future dispatch
//!   loop OR from a one-shot CLI verb.
//! - Outputs are a structured `CompiledAgentOutcome` plus
//!   side-effects on `state.jsonl` + `usage.jsonl` +
//!   optional `alerts.jsonl` (the operator-visible plumbing
//!   halo already owns).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde_json::json;

use pi_sdk::cost::CostRegistry;

use crate::halo::config::{CompiledAgentSpec, ExitPolicy};
use crate::halo::jsonl::{cycle_spend, CycleSpendError};
use crate::halo::state;
use crate::halo::subprocess::{spawn_cycle_subprocess, CycleSubprocessCommand, SubprocessError};

/// Result of one compiled-agent cycle execution. Carries enough
/// info for the future Phase 2c dispatch loop to decide whether
/// to delay the next cycle (Throttle), continue (Continue), or
/// emit an alert (Alert).
#[derive(Debug)]
pub struct CompiledAgentOutcome {
    /// Resolved on_exit policy after looking up the actual exit
    /// code in `spec.on_exit` (with `"*"` wildcard fallback,
    /// then default `Alert` per RFD §D.6).
    pub policy: ExitPolicy,
    pub exit_code: i32,
    /// Precise spend in USD per RFD §D.5. `None` if the agent
    /// emitted no `SessionStarted` event (a wire-format
    /// violation; recorded as `CycleSpendError::NoSessionStarted`).
    pub spend_usd: Option<f64>,
    pub events_parsed: usize,
    /// True if the cycle was aborted by signal_received going high
    /// (operator pressed ^C). Implies policy = Alert (signaled is
    /// always an alert-worthy event).
    pub signaled: bool,
    /// True if the cycle hit its `timeout_secs` cap. Implies the
    /// policy lookup uses exit code 124 (the canonical
    /// "wall-clock-cap-exceeded" code per RFD §D.2).
    pub timed_out: bool,
    pub stderr_tail: String,
}

#[derive(Debug)]
pub struct CycleInputs<'a> {
    pub spec: &'a CompiledAgentSpec,
    /// Directory containing `halo.toml` — used as the anchor for
    /// the spec's relative `binary` path resolution.
    pub halo_toml_parent: &'a Path,
    /// Working directory passed to the child process. Typically
    /// the halo-owned clone per RFD 0025 §259.
    pub cwd: &'a Path,
    pub pricing: &'a CostRegistry,
    pub pid_shared: Arc<AtomicI32>,
    pub signal_received: Arc<AtomicBool>,
    /// Halo's `state.jsonl` for cycle events.
    pub state_jsonl: &'a Path,
    /// Halo's `usage.jsonl` for spend ledger.
    pub usage_jsonl: &'a Path,
    /// Halo's `alerts.jsonl`. Created on-demand when the policy
    /// resolves to `Alert`. (RFD 0025 mentions this file; halo's
    /// existing alert plumbing writes here too.)
    pub alerts_jsonl: &'a Path,
    pub cycle_n: u64,
}

/// Execute one compiled-agent cycle end-to-end.
///
/// Returns the resolved `CompiledAgentOutcome` even on subprocess
/// errors — the caller (Phase 2c dispatch) decides whether to
/// pause halo entirely. Spawn-time failures (binary not found,
/// EACCES) ARE returned as `Err(CompiledAgentError)` since they
/// indicate operator misconfiguration that needs visibility.
pub fn run_compiled_agent_cycle(
    inputs: &CycleInputs<'_>,
) -> Result<CompiledAgentOutcome, CompiledAgentError> {
    let spec = inputs.spec;
    let resolved_binary = spec.resolve_binary(inputs.halo_toml_parent);

    // Halo ALWAYS forces --jsonl per RFD §D.2 (without it spend
    // attribution can't run). The operator's args are appended
    // after; if they ALSO listed --jsonl the duplicate is harmless
    // (Commit B's parser uses `any(|a| a == "--jsonl")`).
    let mut full_args: Vec<String> = Vec::with_capacity(spec.args.len() + 1);
    full_args.push("--jsonl".into());
    full_args.extend(spec.args.iter().cloned());

    let cmd = CycleSubprocessCommand {
        name: &spec.name,
        binary: &resolved_binary,
        args: &full_args,
        prompt: &spec.prompt,
        cwd: inputs.cwd,
        env_extra: &spec.env_extra,
        timeout: spec.timeout(),
        pid_shared: inputs.pid_shared.clone(),
        signal_received: inputs.signal_received.clone(),
    };

    let outcome = spawn_cycle_subprocess(&cmd).map_err(CompiledAgentError::Spawn)?;

    // Resolve exit-code → policy. If signaled or timed_out,
    // synthesize the canonical exit code (130 / 124) for the
    // lookup and unconditionally upgrade to Alert at minimum.
    let lookup_code = if outcome.signaled {
        130 // SIGINT exit per POSIX (128 + 2)
    } else if outcome.timed_out {
        124 // GNU coreutils canonical timeout exit per RFD §D.2
    } else {
        outcome.exit_code
    };
    let policy = resolve_policy(&spec.on_exit, lookup_code);
    // Signaled cycles always get Alert at minimum (operator
    // intervened — they should see an alerts.jsonl row).
    let policy = if outcome.signaled && policy == ExitPolicy::Continue {
        ExitPolicy::Alert
    } else {
        policy
    };

    // Spend attribution. NoSessionStarted is recoverable (we
    // log + carry None forward); the cycle still completed in
    // some form so we don't fail the whole call.
    let spend = match cycle_spend(&outcome.events, inputs.pricing) {
        Ok(s) => Some(s),
        Err(CycleSpendError::NoSessionStarted) => {
            tracing::warn!(
                cycle = inputs.cycle_n,
                spec = %spec.name,
                "compiled-agent emitted no SessionStarted; spend = None"
            );
            None
        }
    };

    // state.jsonl row.
    let _ = state::append_step(
        inputs.state_jsonl,
        inputs.cycle_n,
        "compiled_agent",
        "STEP_COMPILED_AGENT_DONE",
        json!({
            "spec_name": spec.name,
            "binary": resolved_binary.display().to_string(),
            "exit_code": outcome.exit_code,
            "lookup_code": lookup_code,
            "policy": policy_str(policy),
            "events_parsed": outcome.events.len(),
            "spend_usd": spend,
            "wall_seconds": outcome.wall_time.as_secs_f64(),
            "signaled": outcome.signaled,
            "timed_out": outcome.timed_out,
        }),
    );

    // usage.jsonl row (precise per RFD §D.5).
    if let Some(usd) = spend {
        let _ = append_usage_row(
            inputs.usage_jsonl,
            inputs.cycle_n,
            &spec.name,
            usd,
            outcome.wall_time.as_secs_f64(),
        );
    }

    // alerts.jsonl row when policy is Alert.
    if policy == ExitPolicy::Alert {
        let _ = append_alert_row(
            inputs.alerts_jsonl,
            inputs.cycle_n,
            &spec.name,
            outcome.exit_code,
            &outcome.stderr_tail,
        );
    }

    Ok(CompiledAgentOutcome {
        policy,
        exit_code: outcome.exit_code,
        spend_usd: spend,
        events_parsed: outcome.events.len(),
        signaled: outcome.signaled,
        timed_out: outcome.timed_out,
        stderr_tail: outcome.stderr_tail,
    })
}

/// Resolve an exit code against the spec's `on_exit` table.
/// Lookup order:
/// 1. Exact numeric match (`"3"` → ExitPolicy).
/// 2. `"*"` wildcard.
/// 3. Default: `Alert` per RFD §D.6 ("safe-by-default choice").
fn resolve_policy(on_exit: &BTreeMap<String, ExitPolicy>, code: i32) -> ExitPolicy {
    if let Some(p) = on_exit.get(&code.to_string()) {
        return *p;
    }
    if let Some(p) = on_exit.get("*") {
        return *p;
    }
    ExitPolicy::Alert
}

fn policy_str(p: ExitPolicy) -> &'static str {
    match p {
        ExitPolicy::Continue => "continue",
        ExitPolicy::Alert => "alert",
        ExitPolicy::Throttle => "throttle",
    }
}

fn append_usage_row(
    usage_jsonl: &Path,
    cycle_n: u64,
    spec_name: &str,
    spend_usd: f64,
    wall_seconds: f64,
) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let line = json!({
        "ts": Utc::now().to_rfc3339(),
        "cycle": cycle_n,
        "kind": "compiled_agent",
        "spec_name": spec_name,
        "spend_usd": spend_usd,
        "wall_seconds": wall_seconds,
        // The "best_effort_estimated" caveat that orchestrate rows
        // carry per RFD 0025 §552-554 does NOT apply here — this
        // row is computed from the agent's own Usage events.
        "precise": true,
    });
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(usage_jsonl)?;
    let mut s = serde_json::to_string(&line)?;
    s.push('\n');
    f.write_all(s.as_bytes())?;
    Ok(())
}

fn append_alert_row(
    alerts_jsonl: &Path,
    cycle_n: u64,
    spec_name: &str,
    exit_code: i32,
    stderr_tail: &str,
) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let line = json!({
        "ts": Utc::now().to_rfc3339(),
        "cycle": cycle_n,
        "kind": "compiled_agent_alert",
        "spec_name": spec_name,
        "exit_code": exit_code,
        // Cap stderr_tail in the alert row at ~4 KiB so a noisy
        // agent doesn't bloat alerts.jsonl unboundedly. The full
        // 16 KiB tail is in the state.jsonl row + the operator
        // can grep there for the longer context.
        "stderr_tail": &stderr_tail[..stderr_tail.len().min(4096)],
    });
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(alerts_jsonl)?;
    let mut s = serde_json::to_string(&line)?;
    s.push('\n');
    f.write_all(s.as_bytes())?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CompiledAgentError {
    #[error("could not spawn compiled agent: {0}")]
    Spawn(#[from] SubprocessError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn fixture_spec(binary: &str) -> CompiledAgentSpec {
        let mut on_exit = BTreeMap::new();
        on_exit.insert("0".into(), ExitPolicy::Continue);
        on_exit.insert("1".into(), ExitPolicy::Alert);
        on_exit.insert("3".into(), ExitPolicy::Throttle);
        CompiledAgentSpec {
            name: "fixture-cycle".into(),
            binary: binary.into(),
            args: vec![],
            prompt: "test prompt\n".into(),
            on_exit,
            timeout_secs: 0,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 5,
            throttle_base_delay_secs: 60,
            throttle_cap_secs: 3600,
        }
    }

    fn paths_under(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
        (
            tmp.join("state.jsonl"),
            tmp.join("usage.jsonl"),
            tmp.join("alerts.jsonl"),
        )
    }

    fn run<'a>(
        spec: &'a CompiledAgentSpec,
        tmp: &'a Path,
        pricing: &'a CostRegistry,
    ) -> (CompiledAgentOutcome, PathBuf, PathBuf, PathBuf) {
        let (state_jsonl, usage_jsonl, alerts_jsonl) = paths_under(tmp);
        let inputs = CycleInputs {
            spec,
            halo_toml_parent: tmp,
            cwd: tmp,
            pricing,
            pid_shared: Arc::new(AtomicI32::new(0)),
            signal_received: Arc::new(AtomicBool::new(false)),
            state_jsonl: &state_jsonl,
            usage_jsonl: &usage_jsonl,
            alerts_jsonl: &alerts_jsonl,
            cycle_n: 1,
        };
        let outcome = run_compiled_agent_cycle(&inputs).expect("cycle ran");
        (outcome, state_jsonl, usage_jsonl, alerts_jsonl)
    }

    fn agent_script() -> &'static str {
        // Emits SessionStarted + Usage + TurnComplete then exit 0.
        r#"#!/bin/sh
read -r prompt
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"claude-haiku-4-5-20251001","provider":"anthropic"}}'
printf '%s\n' '{"session_id":"s","entry_id":"e2","timestamp":0,"kind":{"type":"usage","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0,"cost_usd":0}}}'
printf '%s\n' '{"session_id":"s","entry_id":"e3","timestamp":0,"kind":{"type":"turn_complete"}}'
exit 0
"#
    }

    #[test]
    fn happy_path_continue_with_precise_spend() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_script());
        let spec = fixture_spec(&bin.display().to_string());

        let mut pricing = CostRegistry::empty();
        pricing.override_for(
            "claude-haiku-4-5-20251001",
            pi_sdk::cost::Pricing::flat(1.0, 5.0),
        );
        let (outcome, state_jsonl, usage_jsonl, alerts_jsonl) = run(&spec, tmp.path(), &pricing);

        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.policy, ExitPolicy::Continue);
        // 1000/1e6 * 1.0 + 500/1e6 * 5.0 = 0.001 + 0.0025 = 0.0035
        let spend = outcome.spend_usd.expect("spend computed");
        assert!((spend - 0.0035).abs() < 1e-9, "spend was {spend}");
        assert!(outcome.events_parsed >= 3);

        // state.jsonl has the cycle event.
        let state_text = std::fs::read_to_string(&state_jsonl).expect("state written");
        assert!(state_text.contains("STEP_COMPILED_AGENT_DONE"));
        assert!(state_text.contains("\"policy\":\"continue\""));

        // usage.jsonl has the precise row.
        let usage_text = std::fs::read_to_string(&usage_jsonl).expect("usage written");
        assert!(usage_text.contains("\"precise\":true"));
        assert!(usage_text.contains("\"kind\":\"compiled_agent\""));

        // alerts.jsonl NOT created on Continue.
        assert!(
            !alerts_jsonl.exists(),
            "alerts file must not be created on Continue policy"
        );
    }

    #[test]
    fn nonzero_exit_routes_to_alert_and_writes_alerts_jsonl() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf 'bad thing happened\n' >&2
exit 1
"#,
        );
        let spec = fixture_spec(&bin.display().to_string());
        let (outcome, _, _, alerts_jsonl) = run(&spec, tmp.path(), &CostRegistry::empty());

        assert_eq!(outcome.exit_code, 1);
        assert_eq!(outcome.policy, ExitPolicy::Alert);
        let alerts = std::fs::read_to_string(&alerts_jsonl).expect("alerts written");
        assert!(alerts.contains("\"kind\":\"compiled_agent_alert\""));
        assert!(alerts.contains("\"exit_code\":1"));
        assert!(alerts.contains("bad thing happened"));
    }

    #[test]
    fn exit_3_routes_to_throttle() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
exit 3
"#,
        );
        let spec = fixture_spec(&bin.display().to_string());
        let (outcome, state_jsonl, _, _) = run(&spec, tmp.path(), &CostRegistry::empty());
        assert_eq!(outcome.policy, ExitPolicy::Throttle);
        let state = std::fs::read_to_string(&state_jsonl).unwrap();
        assert!(state.contains("\"policy\":\"throttle\""));
    }

    #[test]
    fn unspecified_exit_code_defaults_to_alert() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
exit 42
"#,
        );
        let spec = fixture_spec(&bin.display().to_string());
        // 42 is not in spec.on_exit and there's no "*" → defaults Alert.
        let (outcome, _, _, _) = run(&spec, tmp.path(), &CostRegistry::empty());
        assert_eq!(outcome.exit_code, 42);
        assert_eq!(outcome.policy, ExitPolicy::Alert);
    }

    #[test]
    fn wildcard_catch_all_routes_unspecified_codes() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
exit 99
"#,
        );
        let mut spec = fixture_spec(&bin.display().to_string());
        spec.on_exit.insert("*".into(), ExitPolicy::Continue);
        let (outcome, _, _, _) = run(&spec, tmp.path(), &CostRegistry::empty());
        // 99 is unspecified; "*" → Continue.
        assert_eq!(outcome.policy, ExitPolicy::Continue);
    }

    #[test]
    fn no_session_started_event_returns_spend_none() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(
            tmp.path(),
            "agent",
            // Emits Usage WITHOUT SessionStarted — wire violation.
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"s","entry_id":"e","timestamp":0,"kind":{"type":"usage","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0,"cost_usd":0}}}'
exit 0
"#,
        );
        let spec = fixture_spec(&bin.display().to_string());
        let (outcome, _, usage_jsonl, _) = run(&spec, tmp.path(), &CostRegistry::empty());
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.spend_usd.is_none(), "spend must be None on NoSessionStarted");
        // usage.jsonl NOT written when spend is None.
        assert!(
            !usage_jsonl.exists(),
            "usage.jsonl must not be appended when spend is unattributable"
        );
    }

    #[test]
    fn signaled_cycle_upgrades_continue_policy_to_alert() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", "#!/bin/sh\nsleep 30\nexit 0\n");
        let mut spec = fixture_spec(&bin.display().to_string());
        // Map exit 130 (SIGINT) to Continue — we want to verify the
        // signaled-upgrade-to-Alert behavior overrides this.
        spec.on_exit.insert("130".into(), ExitPolicy::Continue);

        let (state_jsonl, usage_jsonl, alerts_jsonl) = paths_under(tmp.path());
        let signal = Arc::new(AtomicBool::new(false));
        let signal_setter = signal.clone();

        // Trip the signal flag after 600 ms.
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            signal_setter.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let inputs = CycleInputs {
            spec: &spec,
            halo_toml_parent: tmp.path(),
            cwd: tmp.path(),
            pricing: &CostRegistry::empty(),
            pid_shared: Arc::new(AtomicI32::new(0)),
            signal_received: signal,
            state_jsonl: &state_jsonl,
            usage_jsonl: &usage_jsonl,
            alerts_jsonl: &alerts_jsonl,
            cycle_n: 1,
        };
        let outcome = run_compiled_agent_cycle(&inputs).expect("cycle ran");
        trigger.join().unwrap();

        assert!(outcome.signaled);
        // signaled-upgrade: Continue → Alert (operator intervened).
        assert_eq!(outcome.policy, ExitPolicy::Alert);
        assert!(alerts_jsonl.exists());
    }

    #[test]
    fn timeout_cycle_uses_124_for_policy_lookup() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", "#!/bin/sh\nsleep 30\nexit 0\n");
        let mut spec = fixture_spec(&bin.display().to_string());
        spec.timeout_secs = 1; // 1 second cap.
        spec.on_exit.insert("124".into(), ExitPolicy::Throttle);

        let (outcome, state_jsonl, _, _) = run(&spec, tmp.path(), &CostRegistry::empty());
        assert!(outcome.timed_out);
        assert_eq!(outcome.policy, ExitPolicy::Throttle);
        let state = std::fs::read_to_string(&state_jsonl).unwrap();
        assert!(state.contains("\"timed_out\":true"));
        assert!(state.contains("\"lookup_code\":124"));
    }

    #[test]
    fn binary_path_resolves_relative_to_halo_toml_parent() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        // Place the binary in a `bin/` subdirectory; spec uses
        // a relative `./bin/agent` form. Should resolve against
        // halo_toml_parent (= tmp.path()), NOT against cwd.
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        write_script(&bin_dir, "agent", agent_script());

        let mut spec = fixture_spec("./bin/agent");
        // Set timeout to 0 (no cap) since `agent_script` runs fast.
        spec.timeout_secs = 0;

        let (outcome, _, _, _) = run(&spec, tmp.path(), &CostRegistry::empty());
        assert_eq!(outcome.exit_code, 0, "binary must resolve relative to halo_toml_parent");
    }
}
