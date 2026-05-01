use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::flag as signal_flag;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::halo::{config, cycle, state};

pub fn halo_dir_for_repo(repo_root: &Path) -> Option<PathBuf> { cycle::halo_dir_for_repo(repo_root) }

fn lock_path(halo_dir: &Path) -> PathBuf { halo_dir.join("lock") }
fn paused_path(halo_dir: &Path) -> PathBuf { halo_dir.join("paused") }
fn pause_req_path(halo_dir: &Path) -> PathBuf { halo_dir.join("pause.req") }
fn stop_req_path(halo_dir: &Path) -> PathBuf { halo_dir.join("stop.req") }

pub fn run_supervisor(repo_root: &Path, config_path: Option<&Path>, max_cycles: u64) -> Result<()> {
    let cfg_path_buf = repo_root.join(".pi/halo.toml");
    let cfg_path = config_path.unwrap_or(cfg_path_buf.as_path());
    let cfg = config::parse(&fs::read_to_string(cfg_path)?)?;
    let errs = config::validate(&cfg, false); if !errs.is_empty() { bail!("config validation errors: {:?}", errs); }
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    fs::create_dir_all(&halo_dir)?;
    if paused_path(&halo_dir).exists() { bail!("halo is paused; run `pi --halo-resume` first"); }
    let sig_any = Arc::new(AtomicBool::new(false)); let sigint = Arc::new(AtomicBool::new(false)); let sigterm = Arc::new(AtomicBool::new(false));
    signal_flag::register(SIGINT, Arc::clone(&sig_any))?; signal_flag::register(SIGTERM, Arc::clone(&sig_any))?; signal_flag::register(SIGINT, Arc::clone(&sigint))?; signal_flag::register(SIGTERM, Arc::clone(&sigterm))?;
    let orchestrate_pid_shared = Arc::new(AtomicI32::new(0));
    startup_reconciliation(repo_root, &halo_dir)?; cycle::prune_old_cycle_branches(repo_root, cfg.cycle.keep_branches);
    let mut cycle_n = 1u64;
    loop {
        if max_cycles != 0 && cycle_n > max_cycles { break; }
        if sig_any.load(Ordering::SeqCst) { std::process::exit(130); }
        maybe_wait_guardrails(repo_root, &halo_dir, &cfg, &mut cycle_n)?;
        if sig_any.load(Ordering::SeqCst) { std::process::exit(130); }
        if paused_path(&halo_dir).exists() { break; }
        let ctx = cycle::build_ctx(repo_root, &halo_dir, cycle_n, &cfg, sig_any.clone(), sigint.clone(), sigterm.clone(), orchestrate_pid_shared.clone());
        match cycle::run_cycle_with_ctx(repo_root, cycle_n, ctx) { Ok(_) | Err(_) => {} }
        if pause_req_path(&halo_dir).exists() { let _ = fs::rename(pause_req_path(&halo_dir), paused_path(&halo_dir)); break; }
        if stop_req_path(&halo_dir).exists() { let _ = fs::remove_file(stop_req_path(&halo_dir)); let _ = fs::remove_file(lock_path(&halo_dir)); break; }
        cycle_n += 1;
    }
    Ok(())
}

fn maybe_wait_guardrails(repo_root: &Path, halo_dir: &Path, cfg: &config::Config, cycle_n: &mut u64) -> Result<()> {
    if in_quiet_hours(&cfg.guardrails.quiet_hours_utc)?
        || cycles_today(halo_dir)? >= cfg.guardrails.cycles_per_day_max && cfg.guardrails.cycles_per_day_max != 0
    {
        wait_for_window_end_or_flags(halo_dir, cfg)?;
    }
    Ok(())
}

fn cycles_today(halo_dir: &Path) -> Result<u32> {
    let midnight = Utc::now().date_naive().and_hms_opt(0,0,0).unwrap().and_utc();
    Ok(state::parse_state_events(&halo_dir.join("state.jsonl")).into_iter().filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("meta") && v.get("meta").and_then(|m| m.as_str()) == Some("CYCLE_DONE") && v.get("ts").and_then(|ts| ts.as_str()).and_then(|ts| DateTime::parse_from_rfc3339(ts).ok()).map(|t| t.with_timezone(&Utc) >= midnight).unwrap_or(false)).count() as u32)
}

fn in_quiet_hours(spec: &str) -> Result<bool> { if spec.trim().is_empty() { return Ok(false); } let (a,b)=spec.split_once('-').ok_or_else(|| anyhow::anyhow!("bad quiet_hours_utc"))?; let now = Utc::now().time(); let start = parse_hm(a)?; let end = parse_hm(b)?; Ok(if start <= end { now >= start && now < end } else { now >= start || now < end }) }
fn parse_hm(s:&str)->Result<chrono::NaiveTime>{ Ok(chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M")?) }

fn wait_for_window_end_or_flags(halo_dir: &Path, cfg: &config::Config) -> Result<()> {
    for _ in 0..60 {
        if paused_path(halo_dir).exists() || pause_req_path(halo_dir).exists() || stop_req_path(halo_dir).exists() { break; }
        std::thread::sleep(Duration::from_secs(60));
        if cfg.guardrails.quiet_hours_utc.trim().is_empty() { break; }
    }
    Ok(())
}

pub fn startup_reconciliation(_repo_root: &Path, _halo_dir: &Path) -> Result<()> { Ok(()) }

fn lock_pid(lock: &Path) -> Option<i32> {
    let text = fs::read_to_string(lock).ok()?;
    text.lines().next()?.trim().parse::<i32>().ok()
}

pub fn operator_pause(repo_root: &Path) -> Result<()> {
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?; fs::create_dir_all(&halo_dir)?; let lock = lock_path(&halo_dir);
    if let Some(pid) = lock_pid(&lock) {
        if pid > 0 {
            let alive = std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false);
            if !alive { let _ = fs::remove_file(&lock); println!("stale lock cleared"); return Ok(()); }
        }
    }
    fs::write(pause_req_path(&halo_dir), b"")?; Ok(())
}

pub fn operator_stop(repo_root: &Path) -> Result<()> {
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?; fs::create_dir_all(&halo_dir)?; fs::write(stop_req_path(&halo_dir), b"")?; Ok(())
}

pub fn operator_resume(repo_root: &Path) -> Result<()> {
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?; let _ = fs::remove_file(paused_path(&halo_dir)); state::append_meta(&halo_dir.join("state.jsonl"), "STREAK_RESET", json!({}))?; Ok(())
}
