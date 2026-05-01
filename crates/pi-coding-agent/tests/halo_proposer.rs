use std::fs;
use std::path::PathBuf;

use pi_coding_agent::halo::{backlog, cycle, proposer};

#[test]
fn proposals_parser_rejects_no_priority_bullet() {
    let response = "## Proposals\n- missing bits\n- keep me (priority: 0.7, est_cost: $1.25, files: a.rs)";
    let out = proposer::parse_proposals(response, 5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "keep me");
}

#[test]
fn backlog_replay_unknown_kind_logged_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backlog.jsonl");
    fs::write(&path, r#"{"kind":"proposal_created","id":"a","title":"t","rationale":"","files_touched":[],"priority":0.1,"est_cost_usd":1.0,"source":"x"}
{"kind":"mystery","id":"a"}
"#).unwrap();
    let map = backlog::replay(&path);
    assert!(map.contains_key("a"));
    assert_eq!(map.len(), 1);
}

#[test]
fn add_proposal_appends_one_event() {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir_all(repo.path().join(".pi")).unwrap();
    fs::write(repo.path().join(".pi").join("halo.toml"), "name='x'\n").unwrap();
    let halo_dir = cycle::halo_dir_for_repo(repo.path()).unwrap();
    fs::create_dir_all(&halo_dir).unwrap();
    std::env::set_current_dir(repo.path()).unwrap();
    pi_coding_agent::cmd::run_halo_add_proposal("t", None, None, Some(0.2), Some(1.0)).unwrap();
    let body = fs::read_to_string(halo_dir.join("backlog.jsonl")).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("proposal_created"));
}

#[test]
fn drop_proposal_appends_one_event_when_safe() {
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir_all(repo.path().join(".pi")).unwrap();
    fs::write(repo.path().join(".pi").join("halo.toml"), "name='x'\n").unwrap();
    let halo_dir = cycle::halo_dir_for_repo(repo.path()).unwrap();
    fs::create_dir_all(&halo_dir).unwrap();
    let backlog_path = halo_dir.join("backlog.jsonl");
    backlog::append_proposal_created(&backlog_path, "id1", "t", "", &[], 0.5, 0.0, "x").unwrap();
    std::env::set_current_dir(repo.path()).unwrap();
    pi_coding_agent::cmd::run_halo_drop_proposal("id1").unwrap();
    let body = fs::read_to_string(&backlog_path).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains("proposal_dropped"));
}

#[test]
fn proposer_cooldown_skips_when_below_min_interval() {
    let repo = tempfile::tempdir().unwrap();
    let halo_dir = cycle::halo_dir_for_repo(repo.path()).unwrap();
    fs::create_dir_all(&halo_dir).unwrap();
    fs::write(halo_dir.join("proposer_last_run"), (chrono::Utc::now().timestamp() - 3600).to_string()).unwrap();
    let cfg = cycle::default_config();
    let res = proposer::run_proposer_if_due(repo.path(), &halo_dir, &halo_dir.join("backlog.jsonl"), &halo_dir.join("state.jsonl"), &cfg, 1, 0).unwrap();
    assert!(res.is_none());
}
