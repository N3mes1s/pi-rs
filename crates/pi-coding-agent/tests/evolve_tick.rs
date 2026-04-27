//! Tests for the evolve-tick primitives (G8): Lock, CostLedger, State,
//! and the should_run gate.

use pi_agent_core::EvolveSettings;
use pi_coding_agent::evolve::{
    is_disabled, should_run, CostLedger, Lock, SkipReason, State, TickDecision,
};

// ─── Lock ─────────────────────────────────────────────────────────────

#[test]
fn lock_acquire_releases_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    {
        let l = Lock::try_acquire(dir.path()).unwrap();
        assert!(l.is_some());
        assert!(dir.path().join(".pi/evolve/lock").exists());
    }
    // Drop deletes the file.
    assert!(!dir.path().join(".pi/evolve/lock").exists());
}

#[test]
fn lock_second_acquire_returns_none_while_first_held() {
    let dir = tempfile::tempdir().unwrap();
    let _first = Lock::try_acquire(dir.path()).unwrap().expect("first ok");
    let second = Lock::try_acquire(dir.path()).unwrap();
    assert!(second.is_none(), "should be blocked");
}

#[test]
fn lock_after_release_can_be_reacquired() {
    let dir = tempfile::tempdir().unwrap();
    {
        let _l = Lock::try_acquire(dir.path()).unwrap().expect("ok");
    }
    let l = Lock::try_acquire(dir.path()).unwrap();
    assert!(l.is_some());
}

#[test]
fn stale_lock_with_dead_pid_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join(".pi/evolve/lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    // Use PID 999_999_999 which won't exist.
    let body = serde_json::json!({
        "pid": 999_999_999u32,
        "acquired_at_ms": chrono::Utc::now().timestamp_millis(),
    });
    std::fs::write(&lock_path, body.to_string()).unwrap();

    // Should reap the stale lock and acquire fresh.
    let l = Lock::try_acquire(dir.path()).unwrap();
    assert!(l.is_some(), "stale dead-PID lock should be reaped");
}

#[test]
fn stale_lock_by_age_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join(".pi/evolve/lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    // Lock with current PID but timestamp 2 hours ago.
    let body = serde_json::json!({
        "pid": std::process::id(),
        "acquired_at_ms": chrono::Utc::now().timestamp_millis() - 2 * 60 * 60 * 1000,
    });
    std::fs::write(&lock_path, body.to_string()).unwrap();

    let l = Lock::try_acquire(dir.path()).unwrap();
    assert!(l.is_some(), "lock older than 1h should be reaped");
}

// ─── CostLedger ───────────────────────────────────────────────────────

#[test]
fn ledger_add_accumulates_today() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = CostLedger::load(dir.path());
    assert_eq!(ledger.today_spend(), 0.0);
    ledger.add(0.05);
    ledger.add(0.10);
    assert!((ledger.today_spend() - 0.15).abs() < 1e-6);
    assert!((ledger.spent_lifetime_usd - 0.15).abs() < 1e-6);
}

#[test]
fn ledger_persists_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = CostLedger::load(dir.path());
    ledger.add(0.42);
    ledger.save(dir.path()).unwrap();

    let ledger2 = CostLedger::load(dir.path());
    assert!((ledger2.spent_today_usd - 0.42).abs() < 1e-6);
    assert!((ledger2.spent_lifetime_usd - 0.42).abs() < 1e-6);
}

#[test]
fn ledger_resets_on_day_rollover() {
    let dir = tempfile::tempdir().unwrap();
    let mut ledger = CostLedger {
        day: "1999-01-01".into(),
        spent_today_usd: 5.0,
        spent_lifetime_usd: 5.0,
    };
    // today_spend should reset because day != today.
    let today = ledger.today_spend();
    assert_eq!(today, 0.0);
    // Lifetime survives.
    assert_eq!(ledger.spent_lifetime_usd, 5.0);
    // Add after rollover bumps today, lifetime still accumulates.
    ledger.add(0.1);
    assert!((ledger.spent_today_usd - 0.1).abs() < 1e-6);
    assert!((ledger.spent_lifetime_usd - 5.1).abs() < 1e-6);
    let _ = dir;
}

// ─── State ────────────────────────────────────────────────────────────

#[test]
fn state_persists_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let s = State {
        last_tick_at_ms: 1234567890,
        outcomes_seen_lifetime: 42,
        outcomes_at_last_tick: 30,
        ticks_run: 3,
    };
    s.save(dir.path()).unwrap();
    let back = State::load(dir.path());
    assert_eq!(back.last_tick_at_ms, 1234567890);
    assert_eq!(back.outcomes_seen_lifetime, 42);
    assert_eq!(back.outcomes_at_last_tick, 30);
    assert_eq!(back.ticks_run, 3);
}

#[test]
fn state_default_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let s = State::load(dir.path());
    assert_eq!(s.last_tick_at_ms, 0);
    assert_eq!(s.outcomes_seen_lifetime, 0);
}

// ─── is_disabled ───────────────────────────────────────────────────────

#[test]
fn disabled_flag_detected() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_disabled(dir.path()));
    let flag = dir.path().join(".pi/evolve/disabled");
    std::fs::create_dir_all(flag.parent().unwrap()).unwrap();
    std::fs::write(&flag, "").unwrap();
    assert!(is_disabled(dir.path()));
}

// ─── should_run ────────────────────────────────────────────────────────

fn settings() -> EvolveSettings {
    EvolveSettings {
        enabled: true,
        daily_cost_cap_usd: 0.50,
        min_samples: 30,
        generations_per_tick: 3,
        benchmark_size: 10,
        min_hours_between_ticks: 24,
        min_new_outcomes_to_retick: 5,
    }
}

fn dir_with_no_disabled() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn should_run_skips_when_disabled_via_settings() {
    let mut s = settings();
    s.enabled = false;
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    let state = State::default();
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, true);
    assert_eq!(decision, TickDecision::Skip(SkipReason::NotEnabled));
}

#[test]
fn should_run_skips_when_per_cwd_disabled_flag_present() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let flag = dir.path().join(".pi/evolve/disabled");
    std::fs::create_dir_all(flag.parent().unwrap()).unwrap();
    std::fs::write(&flag, "").unwrap();
    let mut cost = CostLedger::default();
    let state = State::default();
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, true);
    assert_eq!(decision, TickDecision::Skip(SkipReason::Disabled));
}

#[test]
fn should_run_skips_when_no_agents_md() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    let state = State::default();
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, false);
    assert_eq!(decision, TickDecision::Skip(SkipReason::NoAgentsMd));
}

#[test]
fn should_run_skips_when_below_min_samples() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    let state = State::default();
    let decision = should_run(&s, &mut cost, &state, dir.path(), 5, true);
    assert!(matches!(
        decision,
        TickDecision::Skip(SkipReason::InsufficientSamples { have: 5, need: 30 })
    ));
}

#[test]
fn should_run_skips_when_cost_cap_exceeded() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    cost.add(0.51); // > 0.50 cap
    let state = State::default();
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, true);
    assert_eq!(decision, TickDecision::Skip(SkipReason::CostCapExceeded));
}

#[test]
fn should_run_runs_when_first_time_with_enough_samples() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    let state = State::default(); // last_tick_at_ms = 0 → many hours ago
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, true);
    assert_eq!(decision, TickDecision::Run);
}

#[test]
fn should_run_skips_when_recent_tick_and_few_new_outcomes() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    let state = State {
        last_tick_at_ms: chrono::Utc::now().timestamp_millis() - 60 * 60 * 1000, // 1h ago
        outcomes_at_last_tick: 99,
        outcomes_seen_lifetime: 100,
        ticks_run: 1,
    };
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, true);
    // 1 new outcome (100-99) < 5 needed; 1 hour < 24 needed → skip.
    assert!(matches!(decision, TickDecision::Skip(SkipReason::TooSoon { .. })));
}

#[test]
fn should_run_runs_when_new_outcomes_threshold_hit_even_recently() {
    let s = settings();
    let dir = dir_with_no_disabled();
    let mut cost = CostLedger::default();
    let state = State {
        last_tick_at_ms: chrono::Utc::now().timestamp_millis() - 60 * 60 * 1000,
        outcomes_at_last_tick: 95,
        outcomes_seen_lifetime: 100,
        ticks_run: 1,
    };
    // 5 new outcomes (100-95) >= 5 needed → run despite recent tick.
    let decision = should_run(&s, &mut cost, &state, dir.path(), 100, true);
    assert_eq!(decision, TickDecision::Run);
}
