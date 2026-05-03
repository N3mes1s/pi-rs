use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::flag as signal_flag;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pi_sdk::cost::CostRegistry;

use crate::halo::compiled_agent_dispatch::{
    run_compiled_agent_dispatch, DispatchInputs, SpecRuntimeState,
};
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

    // Per RFD 0028 §D: when [[compiled_agent]] specs are declared,
    // halo runs them AFTER each existing 8-step orchestrate cycle.
    // Throttle state is per-spec, in-memory across ticks; reset on
    // halo restart.
    let mut compiled_agent_states: HashMap<String, SpecRuntimeState> = HashMap::new();
    // Pricing registry for spend attribution. v1 uses pi-sdk's
    // bundled best-effort table; future RFD can let operators
    // override per-model rates in halo.toml.
    let pricing = CostRegistry::with_bundled_defaults();
    // halo.toml's parent directory — anchor for the spec's
    // relative `binary` path resolution per RFD §D.2.
    let halo_toml_parent = cfg_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let alerts_jsonl = halo_dir.join("alerts.jsonl");
    let usage_jsonl = halo_dir.join("usage.jsonl");
    let state_jsonl = halo_dir.join("state.jsonl");
    // v0.27 fix (canary bug #12): continue cycle numbering from
    // the highest CYCLE_DONE/CYCLE_ABORTED in state.jsonl. Prior
    // versions reset cycle_n=1 on every supervisor restart, making
    // it impossible to correlate state.jsonl events across runs
    // (everything showed cycle:1).
    let mut cycle_n = highest_recorded_cycle(&halo_dir).map_or(1, |n| n + 1);
    // v0.27 fix (canary bug #17): max_cycles is operator-RELATIVE,
    // not state.jsonl-ABSOLUTE. Prior versions compared cycle_n
    // (which continues from prior runs after the bug #12 fix) to
    // max_cycles directly, so an operator running halo a second time
    // with `--halo-max-cycles 5` would get 0 cycles (cycle_n already
    // > 5 from yesterday's run). Track the starting cycle and gate
    // on the delta.
    let start_cycle_n = cycle_n;
    let mut last_cycle_start: Option<std::time::Instant> = None;
    loop {
        if max_cycles != 0 && (cycle_n - start_cycle_n) >= max_cycles { break; }
        if sig_any.load(Ordering::SeqCst) { std::process::exit(130); }
        // v0.27 fix (canary bugs #14 + #15): enforce
        // min_seconds_between_cycles. Prior versions parsed it but
        // never honored it, so a NO_PROPOSAL_AVAILABLE cycle (or
        // any fast-skip cycle) would immediately trigger the next
        // one — burning the cycles_per_day_max budget on hot-spin.
        // Honor the gate by sleeping the remainder before each
        // cycle, with paused/pause.req/stop.req checks every second
        // so operator commands aren't blocked.
        if let Some(prev) = last_cycle_start {
            let min = std::time::Duration::from_secs(cfg.guardrails.min_seconds_between_cycles);
            let elapsed = prev.elapsed();
            if elapsed < min {
                let mut remaining = min - elapsed;
                while remaining > std::time::Duration::ZERO {
                    if sig_any.load(Ordering::SeqCst) { std::process::exit(130); }
                    if paused_path(&halo_dir).exists()
                        || pause_req_path(&halo_dir).exists()
                        || stop_req_path(&halo_dir).exists()
                    {
                        break;
                    }
                    let tick = std::cmp::min(remaining, std::time::Duration::from_secs(1));
                    std::thread::sleep(tick);
                    remaining = remaining.saturating_sub(tick);
                }
            }
        }
        maybe_wait_guardrails(&halo_dir, &cfg)?;
        if sig_any.load(Ordering::SeqCst) { std::process::exit(130); }
        if paused_path(&halo_dir).exists() { break; }
        if pause_req_path(&halo_dir).exists() { let _ = fs::rename(pause_req_path(&halo_dir), paused_path(&halo_dir)); break; }
        if stop_req_path(&halo_dir).exists() { let _ = fs::remove_file(stop_req_path(&halo_dir)); let _ = fs::remove_file(lock_path(&halo_dir)); break; }
        last_cycle_start = Some(std::time::Instant::now());
        let ctx = cycle::build_ctx(repo_root, &halo_dir, cycle_n, &cfg, sig_any.clone(), sigint.clone(), sigterm.clone(), orchestrate_pid_shared.clone());
        match cycle::run_cycle_with_ctx(repo_root, cycle_n, ctx) { Ok(_) | Err(_) => {} }

        // Per RFD 0028 §D: after each orchestrate cycle, run the
        // declared compiled-agent specs through the dispatch loop.
        // Empty `compiled_agents` is a no-op (pre-Commit-D
        // behavior — `cfg.compiled_agents` defaults to empty Vec).
        if !cfg.compiled_agents.is_empty() {
            // cwd: prefer the halo-owned clone path per RFD 0025
            // §259 (compiled agents that mutate the working tree
            // should NOT run in the operator's interactive
            // checkout). Fall back to repo_root only when
            // clone.expected_root is unset or empty (covered by
            // validate() which currently requires it set, but
            // belt-and-suspenders).
            let cycle_cwd = cfg
                .clone_config
                .expected_root
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| repo_root.to_path_buf());
            let dispatch_inputs = DispatchInputs {
                specs: &cfg.compiled_agents,
                halo_toml_parent: &halo_toml_parent,
                cwd: &cycle_cwd,
                pricing: &pricing,
                pid_shared: orchestrate_pid_shared.clone(),
                signal_received: sig_any.clone(),
                state_jsonl: &state_jsonl,
                usage_jsonl: &usage_jsonl,
                alerts_jsonl: &alerts_jsonl,
                paused_path: &paused_path(&halo_dir),
                cycle_n,
            };
            // The dispatch returns Err when a spec hits its
            // throttle_streak_max — that ALSO writes the paused
            // file. Propagate by breaking the supervisor loop;
            // operator must `pi --halo-resume` after investigating.
            if let Err(e) = run_compiled_agent_dispatch(&dispatch_inputs, &mut compiled_agent_states) {
                tracing::error!(error = %e, "compiled-agent dispatch paused halo");
                break;
            }
        }

        if pause_req_path(&halo_dir).exists() { let _ = fs::rename(pause_req_path(&halo_dir), paused_path(&halo_dir)); break; }
        if stop_req_path(&halo_dir).exists() { let _ = fs::remove_file(stop_req_path(&halo_dir)); let _ = fs::remove_file(lock_path(&halo_dir)); break; }
        cycle_n += 1;
    }
    Ok(())
}

fn maybe_wait_guardrails(halo_dir: &Path, cfg: &config::Config) -> Result<()> {
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

/// Find the highest cycle number recorded as CYCLE_DONE or
/// CYCLE_ABORTED in `state.jsonl`. Returns `None` for a fresh
/// supervisor (no prior cycles).
fn highest_recorded_cycle(halo_dir: &Path) -> Option<u64> {
    let path = halo_dir.join("state.jsonl");
    let text = fs::read_to_string(&path).ok()?;
    let mut highest: Option<u64> = None;
    for line in text.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("kind").and_then(|k| k.as_str()) != Some("meta") { continue; }
        let meta = v.get("meta").and_then(|m| m.as_str()).unwrap_or("");
        if meta != "CYCLE_DONE" && meta != "CYCLE_ABORTED" { continue; }
        let cycle = v.get("detail").and_then(|d| d.get("cycle")).and_then(|c| c.as_u64());
        if let Some(c) = cycle {
            highest = Some(highest.map_or(c, |h| h.max(c)));
        }
    }
    highest
}

fn lock_pid(lock: &Path) -> Option<i32> {
    let text = fs::read_to_string(lock).ok()?;
    text.lines().next()?.trim().parse::<i32>().ok()
}

pub fn operator_pause(repo_root: &Path) -> Result<()> {
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    fs::create_dir_all(&halo_dir)?;
    let lock = lock_path(&halo_dir);
    if let Some(pid) = lock_pid(&lock) {
        if pid > 0 {
            let alive = std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false);
            if !alive {
                let _ = fs::remove_file(&lock);
                println!("stale lock cleared");
            }
        }
    }
    fs::write(pause_req_path(&halo_dir), b"")?;
    Ok(())
}

pub fn operator_stop(repo_root: &Path) -> Result<()> {
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?; fs::create_dir_all(&halo_dir)?; fs::write(stop_req_path(&halo_dir), b"")?; Ok(())
}

pub fn operator_resume(repo_root: &Path) -> Result<()> {
    let halo_dir = halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?; let _ = fs::remove_file(paused_path(&halo_dir)); state::append_meta(&halo_dir.join("state.jsonl"), "STREAK_RESET", json!({}))?; Ok(())
}
