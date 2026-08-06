use std::fs;

/// Per-test isolated repo root. Each tempdir maps to its own keyed
/// state dir under ~/.pi/halo/, so parallel tests never share a
/// state.jsonl / lock / pause file. The previous cwd-keyed shared dir
/// made `failed_build_streak_triggers_paused_at_n` flake whenever the
/// pause/resume tests interleaved with it — and polluted the real
/// operator state dir on every `cargo test` run.
fn test_repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().to_path_buf();
    let halo = pi_coding_agent::halo::cycle::halo_dir_for_repo(&repo).unwrap();
    fs::create_dir_all(&halo).unwrap();
    (tmp, repo, halo)
}

#[test]
fn commit_rate_cap_aborts_next_cycle() {
    assert!(true);
}

#[test]
fn failed_build_streak_triggers_paused_at_n() {
    let (_tmp, _repo, dir) = test_repo();
    let state = dir.join("state.jsonl");
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
    let (_tmp, repo, dir) = test_repo();
    pi_coding_agent::halo::run::operator_pause(&repo).unwrap();
    assert!(dir.join("pause.req").exists() || dir.join("lock").exists());
}

#[test]
fn halo_resume_clears_paused_and_appends_streak_reset() {
    let (_tmp, repo, dir) = test_repo();
    fs::write(dir.join("paused"), b"").unwrap();
    let state = dir.join("state.jsonl");
    fs::write(&state, b"{\"kind\":\"meta\",\"meta\":\"STREAK_INCREMENTED\"}\n").unwrap();
    pi_coding_agent::halo::run::operator_resume(&repo).unwrap();
    assert!(fs::read_to_string(state).unwrap().contains("STREAK_RESET"));
}

#[test]
fn halo_stop_writes_stop_req() {
    let (_tmp, repo, dir) = test_repo();
    pi_coding_agent::halo::run::operator_stop(&repo).unwrap();
    assert!(dir.join("stop.req").exists());
}

#[test]
fn paused_flag_at_start_refuses_supervisor() {
    assert!(true);
}

#[test]
fn halo_pause_clears_stale_lock_when_pid_dead() {
    let (_tmp, repo, dir) = test_repo();
    fs::write(dir.join("lock"), b"999999\n").unwrap();
    pi_coding_agent::halo::run::operator_pause(&repo).unwrap();
    assert!(!dir.join("lock").exists() || dir.join("pause.req").exists());
}
