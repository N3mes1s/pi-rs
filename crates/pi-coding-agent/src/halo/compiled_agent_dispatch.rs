//! Per RFD 0028 §D.6 — compiled-agent dispatch loop with
//! throttle backoff.
//!
//! Phase 2c: integrates `run_compiled_agent_cycle` (the
//! per-cycle library function) into halo's run.rs cycle-driver
//! loop. Sits AFTER the existing 8-step orchestrate cycle on
//! every tick — purely additive, unchanged orchestrate behavior.
//! When `cfg.compiled_agents` is empty (the pre-Commit-D
//! default), `run_compiled_agent_dispatch` is a no-op.
//!
//! Throttle state is per-spec, in-memory (lost on halo restart;
//! restart resets streaks to 0). When a spec's throttle streak
//! reaches `throttle_streak_max`, halo writes the `paused` file
//! and the dispatch returns an error so the supervisor loop
//! exits cleanly — operator must `pi --halo-resume` after
//! investigating.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use pi_sdk::cost::CostRegistry;

use crate::halo::compiled_agent::{run_compiled_agent_cycle, CycleInputs};
use crate::halo::config::{CompiledAgentSpec, ExitPolicy};

/// Per-spec runtime state. Lives across ticks for a single
/// halo supervisor process; reset on halo restart.
#[derive(Debug, Default, Clone)]
pub struct SpecRuntimeState {
    /// How many consecutive throttle outcomes the spec has had.
    /// Reset to 0 on Continue or Alert.
    pub consecutive_throttles: u32,
    /// If `Some`, the spec is delay-gated until this instant
    /// (exponential backoff per RFD §D.6). `None` = run-now.
    pub next_eligible_at: Option<Instant>,
    /// Total runs (any policy) — for telemetry only.
    pub total_runs: u64,
}

impl SpecRuntimeState {
    /// True if this spec is gated waiting for backoff delay.
    /// Compares `next_eligible_at` against `now` to allow
    /// dependency injection in tests.
    pub fn is_throttle_gated(&self, now: Instant) -> bool {
        self.next_eligible_at.map_or(false, |t| now < t)
    }

    /// Compute the backoff delay for the NEXT throttle event
    /// per RFD §D.6: `min(2^streak * base_delay, cap)` with
    /// `streak = consecutive_throttles` (BEFORE incrementing
    /// for the current event). This is the math the dispatch
    /// applies AFTER incrementing the streak; we expose it as
    /// a pure function so tests can verify the exponent + cap.
    pub fn backoff_delay(streak: u32, base_delay_secs: u64, cap_secs: u64) -> Duration {
        // 2^streak with saturation at u64::MAX (a 64-bit shift
        // overflow at streak >= 64 panics in debug; saturating
        // pow keeps us robust).
        let factor = 2u64.checked_pow(streak).unwrap_or(u64::MAX);
        let raw = factor.saturating_mul(base_delay_secs);
        Duration::from_secs(raw.min(cap_secs))
    }
}

/// Inputs the dispatch loop needs from halo's supervisor.
/// Same shape as `CycleInputs` minus the per-spec bits (which
/// the dispatch loop fills in from the spec list).
pub struct DispatchInputs<'a> {
    pub specs: &'a [CompiledAgentSpec],
    /// Halo.toml's parent dir — anchor for the spec's relative
    /// `binary` path resolution per RFD §D.2.
    pub halo_toml_parent: &'a Path,
    /// Working directory for each spawned cycle. Typically the
    /// halo-owned clone per RFD 0025 §259.
    pub cwd: &'a Path,
    pub pricing: &'a CostRegistry,
    pub pid_shared: Arc<AtomicI32>,
    pub signal_received: Arc<AtomicBool>,
    pub state_jsonl: &'a Path,
    pub usage_jsonl: &'a Path,
    pub alerts_jsonl: &'a Path,
    /// Path to halo's `paused` flag file. Dispatch writes this
    /// when any spec hits its `throttle_streak_max`.
    pub paused_path: &'a Path,
    pub cycle_n: u64,
}

/// Run all eligible compiled-agent specs for one supervisor tick.
///
/// "Eligible" = not currently throttle-gated AND signal_received
/// is not set. Specs are run in `cfg.compiled_agents`
/// declaration order. After each run the spec's runtime state is
/// updated per the resolved `ExitPolicy`:
/// - `Continue` / `Alert` → reset streak, clear gate.
/// - `Throttle` → increment streak, set next_eligible_at to
///   `now + backoff_delay(...)`. If the streak then meets or
///   exceeds `throttle_streak_max`, write the `paused` file and
///   return an error so the supervisor loop exits (operator
///   must `pi --halo-resume` after investigating).
///
/// Empty `specs` slice is a no-op (returns Ok with zero side-
/// effects), matching the RFD §D.2 "if absent, halo's behavior
/// is unchanged from today" contract.
pub fn run_compiled_agent_dispatch(
    inputs: &DispatchInputs<'_>,
    states: &mut HashMap<String, SpecRuntimeState>,
) -> Result<()> {
    let now = Instant::now();
    for spec in inputs.specs {
        if inputs.signal_received.load(std::sync::atomic::Ordering::SeqCst) {
            // Operator pressed ^C — bail out of the rotation.
            // Already-running cycle's signal_received atomic
            // shares the same value; spawn_cycle_subprocess
            // would have killed the child too.
            return Ok(());
        }
        let state = states
            .entry(spec.name.clone())
            .or_default();
        if state.is_throttle_gated(now) {
            tracing::debug!(
                spec = %spec.name,
                "compiled-agent throttle-gated; skipping this tick"
            );
            continue;
        }

        let cycle_inputs = CycleInputs {
            spec,
            halo_toml_parent: inputs.halo_toml_parent,
            cwd: inputs.cwd,
            pricing: inputs.pricing,
            pid_shared: inputs.pid_shared.clone(),
            signal_received: inputs.signal_received.clone(),
            state_jsonl: inputs.state_jsonl,
            usage_jsonl: inputs.usage_jsonl,
            alerts_jsonl: inputs.alerts_jsonl,
            cycle_n: inputs.cycle_n,
        };
        let outcome = match run_compiled_agent_cycle(&cycle_inputs) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(
                    spec = %spec.name,
                    error = %e,
                    "compiled-agent cycle spawn failed; treating as Alert"
                );
                // Spawn failures are treated like Alert — log,
                // continue. The next tick will retry; if the
                // binary is genuinely broken the operator will
                // see the alert log + stop halo.
                state.total_runs = state.total_runs.saturating_add(1);
                state.consecutive_throttles = 0;
                state.next_eligible_at = None;
                continue;
            }
        };

        state.total_runs = state.total_runs.saturating_add(1);
        match outcome.policy {
            ExitPolicy::Continue | ExitPolicy::Alert => {
                state.consecutive_throttles = 0;
                state.next_eligible_at = None;
            }
            ExitPolicy::Throttle => {
                let new_streak = state.consecutive_throttles.saturating_add(1);
                state.consecutive_throttles = new_streak;
                if new_streak >= spec.throttle_streak_max {
                    write_pause_file(
                        inputs.paused_path,
                        &spec.name,
                        new_streak,
                        spec.throttle_streak_max,
                    )?;
                    return Err(anyhow!(
                        "compiled-agent {:?} hit throttle_streak_max ({}); halo paused",
                        spec.name,
                        spec.throttle_streak_max
                    ));
                }
                // Backoff: the streak has been incremented to N;
                // delay by 2^(N-1) * base, capped. Using N-1 (the
                // pre-increment streak) for the exponent matches
                // RFD §D.6 ("2^streak * base_delay" where streak
                // is the count of consecutive throttles INCLUDING
                // the current one's predecessor, so the first
                // throttle delays by `base`, the second by 2*base,
                // etc.).
                let delay = SpecRuntimeState::backoff_delay(
                    new_streak.saturating_sub(1),
                    spec.throttle_base_delay_secs,
                    spec.throttle_cap_secs,
                );
                state.next_eligible_at = Some(now + delay);
                tracing::warn!(
                    spec = %spec.name,
                    streak = new_streak,
                    delay_secs = delay.as_secs(),
                    "compiled-agent throttled; gating next run"
                );
            }
        }
    }
    Ok(())
}

fn write_pause_file(
    paused_path: &Path,
    spec_name: &str,
    streak: u32,
    max: u32,
) -> Result<()> {
    let body = format!(
        "compiled-agent {spec_name:?} hit throttle_streak_max ({streak}/{max}). \
         Investigate alerts.jsonl, then `pi --halo-resume` to continue.\n"
    );
    std::fs::write(paused_path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicI32;

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn fixture_spec(name: &str, binary: &str, exit_policy_for_zero: ExitPolicy) -> CompiledAgentSpec {
        let mut on_exit = BTreeMap::new();
        on_exit.insert("0".into(), exit_policy_for_zero);
        on_exit.insert("3".into(), ExitPolicy::Throttle);
        CompiledAgentSpec {
            name: name.into(),
            binary: binary.into(),
            args: vec![],
            prompt: "test\n".into(),
            on_exit,
            timeout_secs: 0,
            env_extra: BTreeMap::new(),
            throttle_streak_max: 3,
            throttle_base_delay_secs: 1,
            throttle_cap_secs: 60,
        }
    }

    fn agent_continue_script() -> &'static str {
        r#"#!/bin/sh
read -r p
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
printf '%s\n' '{"session_id":"s","entry_id":"e2","timestamp":0,"kind":{"type":"turn_complete"}}'
exit 0
"#
    }

    fn agent_throttle_script() -> &'static str {
        r#"#!/bin/sh
read -r p
printf '%s\n' '{"session_id":"s","entry_id":"e1","timestamp":0,"kind":{"type":"session_started","id":"s","cwd":"/x","model":"m","provider":"p"}}'
exit 3
"#
    }

    fn dispatch_inputs<'a>(
        specs: &'a [CompiledAgentSpec],
        tmp: &'a Path,
        pricing: &'a CostRegistry,
        signal: Arc<AtomicBool>,
        state_jsonl: &'a Path,
        usage_jsonl: &'a Path,
        alerts_jsonl: &'a Path,
        paused_path: &'a Path,
    ) -> DispatchInputs<'a> {
        DispatchInputs {
            specs,
            halo_toml_parent: tmp,
            cwd: tmp,
            pricing,
            pid_shared: Arc::new(AtomicI32::new(0)),
            signal_received: signal,
            state_jsonl,
            usage_jsonl,
            alerts_jsonl,
            paused_path,
            cycle_n: 1,
        }
    }

    // ── Pure backoff-math tests ────────────────────────────────

    #[test]
    fn backoff_delay_streak_zero_returns_base() {
        let d = SpecRuntimeState::backoff_delay(0, 60, 3600);
        assert_eq!(d, Duration::from_secs(60));
    }

    #[test]
    fn backoff_delay_doubles_per_streak() {
        // 2^0*60=60, 2^1*60=120, 2^2*60=240, 2^3*60=480
        assert_eq!(SpecRuntimeState::backoff_delay(0, 60, 3600), Duration::from_secs(60));
        assert_eq!(SpecRuntimeState::backoff_delay(1, 60, 3600), Duration::from_secs(120));
        assert_eq!(SpecRuntimeState::backoff_delay(2, 60, 3600), Duration::from_secs(240));
        assert_eq!(SpecRuntimeState::backoff_delay(3, 60, 3600), Duration::from_secs(480));
    }

    #[test]
    fn backoff_delay_caps_at_cap_secs() {
        // 2^10*60=61440, capped at 3600.
        let d = SpecRuntimeState::backoff_delay(10, 60, 3600);
        assert_eq!(d, Duration::from_secs(3600));
    }

    #[test]
    fn backoff_delay_robust_to_streak_64_overflow() {
        // 2^64 overflows u64; impl saturates. Should not panic.
        let d = SpecRuntimeState::backoff_delay(64, 60, 3600);
        assert_eq!(d, Duration::from_secs(3600));
    }

    // ── Dispatch behavior tests ────────────────────────────────

    #[test]
    fn empty_specs_is_a_noop() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let signal = Arc::new(AtomicBool::new(false));
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();
        let inputs = dispatch_inputs(
            &[],
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        let mut states = HashMap::new();
        run_compiled_agent_dispatch(&inputs, &mut states).expect("noop should succeed");
        assert!(states.is_empty());
        assert!(!state_p.exists()); // no side effects
        assert!(!paused_p.exists());
    }

    #[test]
    fn continue_outcome_resets_streak_and_clears_gate() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_continue_script());
        let spec = fixture_spec("ok-cycle", &bin.display().to_string(), ExitPolicy::Continue);
        let signal = Arc::new(AtomicBool::new(false));
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();
        let inputs = dispatch_inputs(
            std::slice::from_ref(&spec),
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        // Pre-load a state with an existing streak BUT a past
        // (already-elapsed) gate. The dispatch should run the
        // spec, Continue should reset both fields. (A future
        // gate would skip the spec entirely — that's covered
        // by `throttle_gated_spec_skipped_until_next_eligible_at`.)
        let mut states = HashMap::new();
        states.insert(
            "ok-cycle".into(),
            SpecRuntimeState {
                consecutive_throttles: 2,
                next_eligible_at: Some(Instant::now() - Duration::from_secs(1)),
                total_runs: 5,
            },
        );
        run_compiled_agent_dispatch(&inputs, &mut states).unwrap();
        let s = &states["ok-cycle"];
        assert_eq!(s.consecutive_throttles, 0);
        assert!(s.next_eligible_at.is_none());
        assert_eq!(s.total_runs, 6);
    }

    #[test]
    fn throttle_outcome_increments_streak_and_sets_gate() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_throttle_script());
        let spec = fixture_spec("flaky", &bin.display().to_string(), ExitPolicy::Continue);
        let signal = Arc::new(AtomicBool::new(false));
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();
        let inputs = dispatch_inputs(
            std::slice::from_ref(&spec),
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        let mut states = HashMap::new();
        run_compiled_agent_dispatch(&inputs, &mut states).expect("first throttle fine");
        let s = &states["flaky"];
        assert_eq!(s.consecutive_throttles, 1);
        assert!(s.next_eligible_at.is_some());
        assert!(!paused_p.exists(), "first throttle should not pause");
    }

    #[test]
    fn throttle_streak_max_writes_pause_file_and_errors() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_throttle_script());
        let mut spec = fixture_spec("doomed", &bin.display().to_string(), ExitPolicy::Continue);
        spec.throttle_streak_max = 2;
        let signal = Arc::new(AtomicBool::new(false));
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();

        // Pre-load with streak=1 so the next throttle hits max=2.
        let mut states = HashMap::new();
        states.insert(
            "doomed".into(),
            SpecRuntimeState {
                consecutive_throttles: 1,
                next_eligible_at: None,
                total_runs: 1,
            },
        );
        let inputs = dispatch_inputs(
            std::slice::from_ref(&spec),
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        let err = run_compiled_agent_dispatch(&inputs, &mut states).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("throttle_streak_max"), "{msg}");
        assert!(paused_p.exists(), "pause file must be written");
        let body = std::fs::read_to_string(&paused_p).unwrap();
        assert!(body.contains("doomed"));
        assert!(body.contains("halo-resume"));
    }

    #[test]
    fn throttle_gated_spec_skipped_until_next_eligible_at() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_continue_script());
        let spec = fixture_spec("gated", &bin.display().to_string(), ExitPolicy::Continue);
        let signal = Arc::new(AtomicBool::new(false));
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();
        let mut states = HashMap::new();
        // Set next_eligible_at far in the future.
        states.insert(
            "gated".into(),
            SpecRuntimeState {
                consecutive_throttles: 1,
                next_eligible_at: Some(Instant::now() + Duration::from_secs(3600)),
                total_runs: 0,
            },
        );
        let inputs = dispatch_inputs(
            std::slice::from_ref(&spec),
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        run_compiled_agent_dispatch(&inputs, &mut states).unwrap();
        // Spec was NOT run — total_runs unchanged from 0.
        assert_eq!(states["gated"].total_runs, 0);
        // state.jsonl NOT written.
        assert!(!state_p.exists());
    }

    #[test]
    fn signal_received_short_circuits_dispatch() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_continue_script());
        let spec = fixture_spec("ok", &bin.display().to_string(), ExitPolicy::Continue);
        let signal = Arc::new(AtomicBool::new(true)); // pre-tripped
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();
        let inputs = dispatch_inputs(
            std::slice::from_ref(&spec),
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        let mut states = HashMap::new();
        run_compiled_agent_dispatch(&inputs, &mut states).unwrap();
        // Signal pre-tripped → no run, no state inserted.
        assert!(states.is_empty());
        assert!(!state_p.exists());
    }

    #[test]
    fn multiple_specs_run_in_declaration_order() {
        let tmp = tempfile::tempdir_in("/home/nemesis/code").unwrap();
        let bin = write_script(tmp.path(), "agent", agent_continue_script());
        let bin_path = bin.display().to_string();
        let specs = vec![
            fixture_spec("first", &bin_path, ExitPolicy::Continue),
            fixture_spec("second", &bin_path, ExitPolicy::Continue),
            fixture_spec("third", &bin_path, ExitPolicy::Continue),
        ];
        let signal = Arc::new(AtomicBool::new(false));
        let state_p = tmp.path().join("state.jsonl");
        let usage_p = tmp.path().join("usage.jsonl");
        let alerts_p = tmp.path().join("alerts.jsonl");
        let paused_p = tmp.path().join("paused");
        let pricing = CostRegistry::empty();
        let inputs = dispatch_inputs(
            &specs,
            tmp.path(),
            &pricing,
            signal,
            &state_p,
            &usage_p,
            &alerts_p,
            &paused_p,
        );
        let mut states = HashMap::new();
        run_compiled_agent_dispatch(&inputs, &mut states).unwrap();
        assert_eq!(states.len(), 3);
        for name in ["first", "second", "third"] {
            assert_eq!(states[name].total_runs, 1, "{name}");
        }
        // state.jsonl has three rows.
        let lines = std::fs::read_to_string(&state_p)
            .unwrap()
            .lines()
            .count();
        assert_eq!(lines, 3);
    }
}
