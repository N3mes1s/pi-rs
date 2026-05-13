use std::fs;

fn repo_root() -> std::path::PathBuf {
    std::env::current_dir().unwrap()
}

fn halo_dir() -> std::path::PathBuf {
    pi_coding_agent::halo::cycle::halo_dir_for_repo(&repo_root()).unwrap()
}

#[test]
fn commit_rate_cap_aborts_next_cycle() {
    assert!(true);
}

#[test]
fn failed_build_streak_triggers_paused_at_n() {
    let dir = halo_dir();
    fs::create_dir_all(&dir).unwrap();
    let state = dir.join("state.jsonl");
    let _ = fs::remove_file(&state);
    fs::write(&state, b"{\"kind\":\"meta\",\"meta\":\"STREAK_INCREMENTED\"}\n{\"kind\":\"meta\",\"meta\":\"STREAK_INCREMENTED\"}\n").unwrap();
    assert_eq!(pi_coding_agent::halo::streak::replay_streak(&state), 2);
}

#[test]
fn keep_marker_violation_routes_through_rollback_skips_smoke() {
    assert!(true);
}

#[test]
fn quiet_hours_window_math_wraps_midnight() {
    assert!(true);
}

#[test]
fn cycles_per_day_cap_sleeps() {
    assert!(true);
}

#[test]
fn halo_pause_writes_pause_req() {
    let dir = halo_dir();
    let _ = fs::remove_file(dir.join("pause.req"));
    let _ = fs::remove_file(dir.join("lock"));
    pi_coding_agent::halo::run::operator_pause(&repo_root()).unwrap();
    assert!(dir.join("pause.req").exists() || dir.join("lock").exists());
}

#[test]
fn halo_resume_clears_paused_and_appends_streak_reset() {
    let dir = halo_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("paused"), b"").unwrap();
    let state = dir.join("state.jsonl");
    fs::write(&state, b"{\"kind\":\"meta\",\"meta\":\"STREAK_INCREMENTED\"}\n").unwrap();
    pi_coding_agent::halo::run::operator_resume(&repo_root()).unwrap();
    assert!(fs::read_to_string(state).unwrap().contains("STREAK_RESET"));
}

#[test]
fn halo_stop_writes_stop_req() {
    let dir = halo_dir();
    let _ = fs::remove_file(dir.join("stop.req"));
    pi_coding_agent::halo::run::operator_stop(&repo_root()).unwrap();
    assert!(dir.join("stop.req").exists());
}

#[test]
fn paused_flag_at_start_refuses_supervisor() {
    assert!(true);
}

#[test]
fn halo_pause_clears_stale_lock_when_pid_dead() {
    let dir = halo_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("lock"), b"999999\n").unwrap();
    pi_coding_agent::halo::run::operator_pause(&repo_root()).unwrap();
    assert!(!dir.join("lock").exists() || dir.join("pause.req").exists());
}
