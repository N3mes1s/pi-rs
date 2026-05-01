//! Supervisor runner for halo — RFD 0025 M2.
//!
//! Implements: `run_supervisor(repo_root, max_cycles)` which runs up to
//! `max_cycles` cycles (default 1 in M2), installs a `signal_hook` SIGINT/SIGTERM
//! handler that sets a shared flag and sends signals to the orchestrate child
//! PG when triggered, and performs the startup reconciliation pass.

use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::flag as signal_flag;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::halo::{config, state, cycle};

/// Run the supervisor for up to `max_cycles`. `max_cycles == 0` => run forever.
pub fn run_supervisor(repo_root: &Path, config_path: Option<&Path>, max_cycles: u64) -> Result<()> {
    let cfg = config::parse(&std::fs::read_to_string(config_path.unwrap_or(&repo_root.join(".pi/halo.toml")))?)?;

    // Validate config and step set.
    let errs = config::validate(&cfg, false);
    if !errs.is_empty() {
        anyhow::bail!("config validation errors: {:?}", errs);
    }

    // Build halo dir and pid/lock — simplified: write pid file.
    let halo_dir = cycle::halo_dir_for_repo(repo_root).ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    std::fs::create_dir_all(&halo_dir)?;
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
    std::fs::write(halo_dir.join("pid"), format!("{}\n{}\n{}\n", std::process::id(), host, Utc::now().to_rfc3339()))?;

    // Signal handling: set flags on SIGINT/SIGTERM.
    // We register three atomics: `sig_any` (set by either signal), and
    // per-signal flags `sigint` / `sigterm` so the cycle can determine
    // which signal arrived.
    let sig_any = Arc::new(AtomicBool::new(false));
    let sigint = Arc::new(AtomicBool::new(false));
    let sigterm = Arc::new(AtomicBool::new(false));
    // Register `sig_any` for both signals so it's set regardless which one
    // arrives; also register the per-signal flags so we can report the
    // exact signal name in state/backlog events.
    signal_flag::register(SIGINT, Arc::clone(&sig_any))?;
    signal_flag::register(SIGTERM, Arc::clone(&sig_any))?;
    signal_flag::register(SIGINT, Arc::clone(&sigint))?;
    signal_flag::register(SIGTERM, Arc::clone(&sigterm))?;

    // Shared atomic for orchestrate child PID so the handler can kill PG.
    let orchestrate_pid_shared = Arc::new(AtomicI32::new(0));

    // Spawn a background watcher that reacts to the signal flags by
    // propagating the signal to the orchestrate child's process group,
    // waiting up to `interrupt_grace_seconds`, then escalating to SIGKILL.
    {
        let orchestrate_pid_shared = orchestrate_pid_shared.clone();
        let sigint_w = sigint.clone();
        let sigterm_w = sigterm.clone();
        let sigany_w = sig_any.clone();
        let grace = cfg.supervisor.interrupt_grace_seconds;
        std::thread::spawn(move || {
            // Wait for any signal to be set.
            loop {
                if sigint_w.load(Ordering::SeqCst) || sigterm_w.load(Ordering::SeqCst) {
                    // Determine which signal arrived.
                    let sig_num = if sigint_w.load(Ordering::SeqCst) { SIGINT } else { SIGTERM };
                    // Capture the PG leader PID as observed when the signal
                    // arrived.
                    let pid = orchestrate_pid_shared.load(Ordering::SeqCst);
                    if pid != 0 {
                        // Use the external `kill` tool to send the signal to
                        // the negative PID (process group). We pass `--` to
                        // avoid the negative PID being parsed as an option.
                        let _ = std::process::Command::new("kill")
                            .args([format!("-{}", sig_num), "--".to_string(), format!("-{}", pid)])
                            .status();
                        // Wait up to the grace period for the PG to exit.
                        let start = Instant::now();
                        while orchestrate_pid_shared.load(Ordering::SeqCst) != 0 {
                            if start.elapsed().as_secs() >= grace {
                                // Escalate to SIGKILL the PG (kill -9 -- -<pid>).
                                let _ = std::process::Command::new("kill")
                                    .args(["-9".to_string(), "--".to_string(), format!("-{}", pid)])
                                    .status();
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    // Ensure the general flag is set so the supervisor loop can
                    // detect the signal arrival even if the per-signal flag is
                    // cleared elsewhere.
                    sigany_w.store(true, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }

    // Startup reconciliation pass (M2 simplified): if any dispatched proposal
    // lacks a terminal cycle row, re-queue as pending and emit a synthetic
    // CYCLE_ABORTED and STALE_DISPATCHED_RECOVERED meta row.
    startup_reconciliation(repo_root, &halo_dir)?;

    // Prune old cycle branches per config
    cycle::prune_old_cycle_branches(repo_root, cfg.cycle.keep_branches);

    // Main loop: run up to max_cycles cycles.
    let mut cycle_n = 1u64;
    loop {
        if max_cycles != 0 && cycle_n > max_cycles {
            break;
        }
        // If a signal has already arrived, don't start another cycle; exit 130.
        if sig_any.load(Ordering::SeqCst) {
            std::process::exit(130);
        }

        let ctx = cycle::build_ctx(repo_root, &halo_dir, cycle_n, &cfg, sig_any.clone(), sigint.clone(), sigterm.clone(), orchestrate_pid_shared.clone());
        match cycle::run_cycle_with_ctx(repo_root, cycle_n, ctx) {
            Ok(cycle::CycleOutcome::Done { outcome }) => {
                eprintln!("cycle {} done: {}", cycle_n, outcome);
            }
            Ok(cycle::CycleOutcome::Aborted { reason }) => {
                eprintln!("cycle {} aborted: {}", cycle_n, reason);
            }
            Err(e) => {
                eprintln!("cycle {} failed: {}", cycle_n, e);
            }
        }

        cycle_n += 1;
        std::thread::sleep(Duration::from_secs(1));
    }

    Ok(())
}

pub fn startup_reconciliation(_repo_root: &Path, halo_dir: &Path) -> Result<()> {
    let state_p = halo_dir.join("state.jsonl");
    let backlog_p = halo_dir.join("backlog.jsonl");
    let state_events = state::parse_state_events(&state_p);
    let backlog_text = std::fs::read_to_string(&backlog_p).unwrap_or_default();

    // Collect dispatched proposals that lack a cycle terminal, grouped by cycle.
    let mut recovered_by_cycle: std::collections::BTreeMap<u64, Vec<String>> = Default::default();

    for line in backlog_text.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("kind").and_then(|k| k.as_str()) == Some("proposal_status_changed") {
                if v.get("status").and_then(|s| s.as_str()) == Some("dispatched") {
                    // Extract cycle
                    let cycle_n = v.get("detail").and_then(|d| d.get("cycle")).and_then(|c| c.as_u64());
                    if let Some(cn) = cycle_n {
                        if !state::has_cycle_terminal(&state_events, cn) {
                            if let Some(pid) = v.get("id").and_then(|i| i.as_str()) {
                                recovered_by_cycle.entry(cn).or_default().push(pid.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    if recovered_by_cycle.is_empty() {
        return Ok(());
    }

    // Re-queue as pending (one row per proposal) and write per-cycle CYCLE_ABORTED,
    // then emit a single aggregated STALE_DISPATCHED_RECOVERED event covering the window.
    let mut all_proposals: Vec<String> = Vec::new();
    let mut cycle_window_min: Option<u64> = None;
    let mut cycle_window_max: Option<u64> = None;

    for (cn, ids) in &recovered_by_cycle {
        for pid in ids {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&backlog_p)
                .and_then(|mut f| {
                    let evt = serde_json::json!({
                        "kind": "proposal_status_changed",
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "id": pid,
                        "status": "pending",
                        "detail": {"reason": "supervisor_crashed"},
                    });
                    let s = serde_json::to_string(&evt).map(|s| s + "\n");
                    match s {
                        Ok(s) => f.write_all(s.as_bytes()),
                        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
                    }
                });
        }

        // Append one synthetic CYCLE_ABORTED per cycle.
        let _ = state::append_meta(
            &state_p,
            "CYCLE_ABORTED",
            json!({"cycle": cn, "reason": "supervisor_crashed", "recovered": true}),
        );

        all_proposals.extend(ids.clone());
        cycle_window_min = Some(cycle_window_min.map_or(*cn, |m| m.min(*cn)));
        cycle_window_max = Some(cycle_window_max.map_or(*cn, |m| m.max(*cn)));
    }

    // Emit one aggregated STALE_DISPATCHED_RECOVERED event.
    let window = match (cycle_window_min, cycle_window_max) {
        (Some(min), Some(max)) => json!([min, max]),
        _ => json!([]),
    };
    let _ = state::append_meta(&state_p, "STALE_DISPATCHED_RECOVERED", json!({"proposals": all_proposals, "cycle_window": window}));

    Ok(())
}
