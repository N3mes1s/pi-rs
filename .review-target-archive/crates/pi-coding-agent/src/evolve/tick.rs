//! Background evolve-tick: lock + cost ledger + state + run gate (G8).
//!
//! The actual subprocess invocation (`pi --internal-evolve-tick`) and
//! the orchestration that calls [`Mutator`] + [`Replay`] in sequence
//! lands in G9 alongside Pareto/apply logic. This module ships the
//! primitives those higher layers need:
//!
//! - [`Lock`] — single-instance file lock at `<cwd>/.pi/evolve/lock`,
//!   with PID + timestamp. Stale (>1h, dead PID) locks are reaped.
//! - [`CostLedger`] — per-day USD spend tracker at
//!   `<cwd>/.pi/evolve/ledger.json`. Auto-resets on UTC day rollover.
//! - [`State`] — last-tick timestamp + outcome counter at
//!   `<cwd>/.pi/evolve/state.json`.
//! - [`should_run`] — combines settings + state + cost into a
//!   yes/no decision with a structured `SkipReason` for each `no`.
//!
//! Storage layout (per cwd):
//!
//! ```text
//! <cwd>/.pi/evolve/
//!   lock                # exclusive single-instance marker
//!   ledger.json         # cost-per-day ledger
//!   state.json          # last_tick_at_ms, outcomes_seen
//!   generations.jsonl   # one line per (candidate, summary) pair
//!   history/            # backups of replaced AGENTS.md files
//!   disabled            # flag file: disables evolution for this cwd
//! ```

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pi_agent_core::EvolveSettings;

const LOCK_FILE: &str = "lock";
const LEDGER_FILE: &str = "ledger.json";
const STATE_FILE: &str = "state.json";
const DISABLED_FLAG: &str = "disabled";
const STALE_LOCK_AFTER: Duration = Duration::from_secs(60 * 60); // 1h

/// Path to `<cwd>/.pi/evolve/`. Creates the directory on demand.
pub fn evolve_dir(cwd: &Path) -> io::Result<PathBuf> {
    let dir = cwd.join(".pi").join("evolve");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Has the user disabled evolution for this cwd via `pi evolve off`?
pub fn is_disabled(cwd: &Path) -> bool {
    cwd.join(".pi").join("evolve").join(DISABLED_FLAG).exists()
}

/// Why a tick was skipped. The daemon writes this to the generations
/// log so users can see why nothing happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum SkipReason {
    NotEnabled,
    Disabled,
    LockHeld,
    CostCapExceeded,
    InsufficientSamples { have: u32, need: u32 },
    TooSoon { hours_left: u32 },
    NotEnoughNewOutcomes { since: u32, need: u32 },
    NoAgentsMd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TickDecision {
    Run,
    Skip(SkipReason),
}

// ─── Lock ───────────────────────────────────────────────────────────────

/// Single-instance file lock for `<cwd>/.pi/evolve/`.
///
/// Acquisition is a CAS via `OpenOptions::new().create_new(true)`:
/// succeeds iff no lock file exists. Release happens on Drop.
///
/// Stale-lock recovery: if a lock file exists but is older than
/// `STALE_LOCK_AFTER` OR its PID is no longer alive, we delete it and
/// retry once. Daemon crashes therefore don't permanently block.
pub struct Lock {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockBody {
    pid: u32,
    acquired_at_ms: i64,
}

impl Lock {
    pub fn try_acquire(cwd: &Path) -> io::Result<Option<Self>> {
        let dir = evolve_dir(cwd)?;
        let path = dir.join(LOCK_FILE);

        // First attempt.
        if let Some(lock) = Self::create(&path)? {
            return Ok(Some(lock));
        }
        // Lock exists. Is it stale? If yes, reap and retry once.
        if Self::is_stale(&path) {
            tracing::warn!(
                cwd = %cwd.display(),
                lock_path = %path.display(),
                "evolve: reaping stale lock file",
            );
            let _ = fs::remove_file(&path);
            return Self::create(&path);
        }
        Ok(None)
    }

    fn create(path: &Path) -> io::Result<Option<Self>> {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => {
                let body = LockBody {
                    pid: std::process::id(),
                    acquired_at_ms: now_ms(),
                };
                f.write_all(serde_json::to_string(&body).unwrap().as_bytes())?;
                Ok(Some(Self {
                    path: path.to_path_buf(),
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn is_stale(path: &Path) -> bool {
        let Ok(txt) = fs::read_to_string(path) else {
            return true; // unreadable = stale
        };
        let Ok(body) = serde_json::from_str::<LockBody>(&txt) else {
            return true;
        };
        let age = Duration::from_millis((now_ms() - body.acquired_at_ms).max(0) as u64);
        if age > STALE_LOCK_AFTER {
            return true;
        }
        !pid_alive(body.pid)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn pid_alive(_pid: u32) -> bool {
    // Conservative: assume alive on non-Linux platforms. Stale-by-age
    // still kicks in, so we can't deadlock indefinitely.
    true
}

// ─── CostLedger ────────────────────────────────────────────────────────

/// Per-day cost tracking. Persists to `<cwd>/.pi/evolve/ledger.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostLedger {
    /// UTC date of `spent_today_usd`, e.g. "2026-04-27". When the
    /// current day differs, `spent_today_usd` is treated as 0.
    pub day: String,
    pub spent_today_usd: f32,
    /// Lifetime sum, useful for diagnostics.
    #[serde(default)]
    pub spent_lifetime_usd: f32,
}

impl CostLedger {
    pub fn load(cwd: &Path) -> Self {
        let dir = match evolve_dir(cwd) {
            Ok(d) => d,
            Err(_) => return Self::default(),
        };
        let path = dir.join(LEDGER_FILE);
        let txt = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&txt).unwrap_or_default()
    }

    pub fn save(&self, cwd: &Path) -> io::Result<()> {
        let path = evolve_dir(cwd)?.join(LEDGER_FILE);
        let txt = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(path, txt)
    }

    /// Spend `usd` against the day's budget. Auto-resets if the UTC
    /// day has rolled over since last call. Returns the new
    /// `spent_today_usd`.
    pub fn add(&mut self, usd: f32) -> f32 {
        let today = today_utc_string();
        if self.day != today {
            self.day = today;
            self.spent_today_usd = 0.0;
        }
        self.spent_today_usd += usd;
        self.spent_lifetime_usd += usd;
        self.spent_today_usd
    }

    /// Effective spend for today. Auto-resets day if needed.
    pub fn today_spend(&mut self) -> f32 {
        let today = today_utc_string();
        if self.day != today {
            self.day = today;
            self.spent_today_usd = 0.0;
        }
        self.spent_today_usd
    }
}

fn today_utc_string() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

// ─── State ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub last_tick_at_ms: i64,
    /// Total outcome-labelled (non-Replay) trajectories the daemon has
    /// observed for this cwd, ever.
    pub outcomes_seen_lifetime: u32,
    /// Outcome count at last tick. Diff with current = new outcomes.
    pub outcomes_at_last_tick: u32,
    /// Number of ticks executed (run-not-skipped).
    pub ticks_run: u32,
}

impl State {
    pub fn load(cwd: &Path) -> Self {
        let dir = match evolve_dir(cwd) {
            Ok(d) => d,
            Err(_) => return Self::default(),
        };
        let path = dir.join(STATE_FILE);
        let txt = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        serde_json::from_str(&txt).unwrap_or_default()
    }

    pub fn save(&self, cwd: &Path) -> io::Result<()> {
        let path = evolve_dir(cwd)?.join(STATE_FILE);
        let txt = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(path, txt)
    }
}

// ─── should_run gate ───────────────────────────────────────────────────

/// Combine settings + cost + state + sample count + AGENTS.md presence
/// into a single `TickDecision`. Pure: no I/O beyond the lock check
/// (passed in as `held_lock`). The daemon performs the decision *after*
/// acquiring the lock so this fn assumes the lock is held.
pub fn should_run(
    settings: &EvolveSettings,
    cost: &mut CostLedger,
    state: &State,
    cwd: &Path,
    outcome_samples_now: u32,
    has_agents_md: bool,
) -> TickDecision {
    if !settings.enabled {
        return TickDecision::Skip(SkipReason::NotEnabled);
    }
    if is_disabled(cwd) {
        return TickDecision::Skip(SkipReason::Disabled);
    }
    if !has_agents_md {
        return TickDecision::Skip(SkipReason::NoAgentsMd);
    }
    if outcome_samples_now < settings.min_samples {
        return TickDecision::Skip(SkipReason::InsufficientSamples {
            have: outcome_samples_now,
            need: settings.min_samples,
        });
    }
    if cost.today_spend() >= settings.daily_cost_cap_usd {
        return TickDecision::Skip(SkipReason::CostCapExceeded);
    }
    // Rate-limit: either enough hours since last tick OR enough new outcomes.
    let now = now_ms();
    let elapsed_hours = ((now - state.last_tick_at_ms).max(0) / (1000 * 60 * 60)) as u32;
    let new_outcomes = outcome_samples_now.saturating_sub(state.outcomes_at_last_tick);
    let hours_ok = elapsed_hours >= settings.min_hours_between_ticks;
    let outcomes_ok = new_outcomes >= settings.min_new_outcomes_to_retick;
    if !hours_ok && !outcomes_ok {
        if elapsed_hours < settings.min_hours_between_ticks {
            return TickDecision::Skip(SkipReason::TooSoon {
                hours_left: settings.min_hours_between_ticks - elapsed_hours,
            });
        }
        return TickDecision::Skip(SkipReason::NotEnoughNewOutcomes {
            since: new_outcomes,
            need: settings.min_new_outcomes_to_retick,
        });
    }
    TickDecision::Run
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
