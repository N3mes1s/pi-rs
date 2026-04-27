//! Top-level subcommands: install / list / config / update.
//! Used when the `--install`, `--list`, `--config`, or `--update` flags are
//! present (mirroring `pi install`, `pi list`, etc.).

use crate::context::{agent_dir, package_dir};
use crate::packages;

pub fn run_install(spec: &str) -> anyhow::Result<()> {
    let dest = package_dir();
    let pkg = packages::install(spec, &dest)?;
    println!(
        "installed {} ({}) -> {}",
        pkg.name.is_empty().then(|| spec.to_string()).unwrap_or(pkg.name.clone()),
        pkg.version,
        pkg.path.display()
    );
    Ok(())
}

pub fn run_list() -> anyhow::Result<()> {
    let pkgs = packages::discover(&package_dir());
    if pkgs.is_empty() {
        println!("(no packages installed)");
        return Ok(());
    }
    for pkg in pkgs {
        println!("- {} {}  ({})", pkg.name, pkg.version, pkg.path.display());
    }
    Ok(())
}

pub fn run_config() -> anyhow::Result<()> {
    let dir = agent_dir();
    println!("agent dir: {}", dir.display());
    println!("settings:  {}", dir.join("settings.json").display());
    println!("auth:      {}", dir.join("auth.json").display());
    println!("sessions:  {}", dir.join("sessions").display());
    println!("packages:  {}", package_dir().display());
    Ok(())
}

pub fn run_update() -> anyhow::Result<()> {
    let pkgs = packages::discover(&package_dir());
    for pkg in pkgs {
        println!("updating {} …", pkg.name);
        let _ = std::process::Command::new("git")
            .args(["-C"])
            .arg(&pkg.path)
            .arg("pull")
            .status();
    }
    Ok(())
}

/// `pi --internal-evolve-tick` — spawned by the modes' exit hook.
/// Runs one orchestrator pass and exits. Silent on the happy path so
/// the daemon doesn't pollute the user's terminal; logs to
/// `<cwd>/.pi/evolve/tick.log` for diagnostics.
pub async fn run_internal_evolve_tick() -> anyhow::Result<()> {
    use crate::context::{agent_dir, auth_path, sessions_dir, settings_paths};
    use crate::evolve::{evolve_dir, run_tick, SubprocessReplay, TickInputs, TickReport};
    use pi_agent_core::Settings;
    use pi_ai::{AuthStorage, ModelRegistry};
    use std::time::Duration;

    let cwd = std::env::current_dir()?;

    // Load settings (global + project) — same as startup::assemble.
    let (global, project) = settings_paths();
    let mut settings = Settings::load(&global);
    settings.merge_project(&project);

    // Auth: file then env.
    let auth = AuthStorage::open(auth_path()).unwrap_or_else(|_| AuthStorage::in_memory());
    let env = AuthStorage::from_env();
    for (p, _) in AuthStorage::ENV_KEYS {
        if let Some(m) = env.get(p) {
            auth.set(p, m);
        }
    }
    let registry = ModelRegistry::new(auth.clone());

    // Locate AGENTS.md: project first, then ancestors, then global.
    let agents_md_path = locate_agents_md(&cwd, &agent_dir());

    // Tick log file.
    let _ = evolve_dir(&cwd); // ensure dir exists
    let log_path = cwd.join(".pi").join("evolve").join("tick.log");
    let mut log_line = format!("[{}] tick start\n", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"));

    // Pi binary path: prefer current_exe so the subprocess stays
    // consistent across self-replacing installs.
    let pi_binary = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("pi"));
    let replay = SubprocessReplay {
        pi_binary,
        timeout: Duration::from_secs(180),
        auto_approve: "auto-policy".into(),
        cwd: Some(cwd.clone()),
    };

    let inputs = TickInputs {
        cwd: &cwd,
        sessions_root: &sessions_dir(),
        agents_md_path,
        settings: &settings,
        registry: &registry,
        auth: &auth,
    };

    match run_tick(inputs, &replay).await {
        Ok(TickReport::Skipped(why)) => {
            log_line.push_str(&format!("  skipped: {:?}\n", why));
        }
        Ok(TickReport::Ran {
            baseline,
            generations,
            applied_hash,
        }) => {
            log_line.push_str(&format!(
                "  ran: baseline pass={:.2}, {} generations, applied={:?}\n",
                baseline.pass_rate,
                generations.len(),
                applied_hash
            ));
        }
        Err(e) => {
            log_line.push_str(&format!("  error: {}\n", e));
        }
    }

    // Append to log; ignore failures (we're a background process).
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = f.write_all(log_line.as_bytes());
    }
    Ok(())
}

fn locate_agents_md(cwd: &std::path::Path, agent_dir: &std::path::Path) -> std::path::PathBuf {
    // Walk cwd ancestors looking for AGENTS.md.
    for dir in cwd.ancestors() {
        let p = dir.join("AGENTS.md");
        if p.is_file() {
            return p;
        }
    }
    // Fall back to global. May not exist; orchestrator will gate-skip
    // with NoAgentsMd if so.
    agent_dir.join("AGENTS.md")
}

/// `pi --evolve {status,off,on}` — control the autonomous AGENTS.md
/// evolution daemon for the current cwd.
pub fn run_evolve(verb: &str) -> anyhow::Result<()> {
    use crate::evolve::{
        evolve_dir, is_disabled, read_generations, CostLedger, PendingApply, State,
    };

    let cwd = std::env::current_dir()?;
    match verb {
        "off" => {
            let dir = evolve_dir(&cwd)?;
            let flag = dir.join("disabled");
            std::fs::write(&flag, "")?;
            println!("evolve: disabled for {}", cwd.display());
            Ok(())
        }
        "on" => {
            let dir = evolve_dir(&cwd)?;
            let flag = dir.join("disabled");
            if flag.exists() {
                std::fs::remove_file(&flag)?;
            }
            println!("evolve: enabled for {}", cwd.display());
            Ok(())
        }
        "status" => {
            let mut ledger = CostLedger::load(&cwd);
            let state = State::load(&cwd);
            let pending = PendingApply::load(&cwd);
            let gens = read_generations(&cwd);
            let disabled = is_disabled(&cwd);

            println!("evolve status for {}", cwd.display());
            println!(
                "  enabled-here:   {}",
                if disabled {
                    "no (.pi/evolve/disabled)"
                } else {
                    "yes"
                }
            );
            println!("  ticks_run:      {}", state.ticks_run);
            println!(
                "  outcomes_seen:  {} lifetime, {} at last tick",
                state.outcomes_seen_lifetime, state.outcomes_at_last_tick
            );
            println!("  spent_today:    ${:.4}", ledger.today_spend());
            println!("  spent_lifetime: ${:.4}", ledger.spent_lifetime_usd);
            println!("  generations:    {} logged", gens.len());
            if let Some(applied) = gens.iter().rev().find(|g| g.applied) {
                let n = applied.hash.len().min(12);
                println!(
                    "  last_applied:   hash={} note=\"{}\"",
                    &applied.hash[..n],
                    applied.note
                );
            }
            if let Some(p) = pending {
                let n = p.applied_hash.len().min(12);
                println!(
                    "  pending_apply:  hash={} (rollback monitor watching)",
                    &p.applied_hash[..n]
                );
            }
            Ok(())
        }
        other => anyhow::bail!("unknown --evolve verb: {other} (expected: status, off, on)"),
    }
}

/// `pi --flamegraph <session-id-or-path>` — render trajectory flamegraph
/// to HTML on stdout. Width = estimated tokens, depth = turn nesting,
/// colour = block kind. Self-contained (no JS, no external assets).
pub fn run_flamegraph(target: &str) -> anyhow::Result<()> {
    use crate::context::sessions_dir;
    use crate::native::trajectory::flamegraph;
    use pi_agent_core::SessionEntry;

    // Resolve the input: a path, or a session id under the per-cwd
    // sessions directory.
    let path = if std::path::Path::new(target).is_file() {
        std::path::PathBuf::from(target)
    } else {
        let cwd = std::env::current_dir()?;
        let slug = cwd.display().to_string().replace(['/', '\\', ':'], "_");
        let dir = sessions_dir().join(slug);
        let candidate = dir.join(format!("{target}.jsonl"));
        if !candidate.exists() {
            anyhow::bail!(
                "no session jsonl at {} (looked up id={} for cwd={})",
                candidate.display(),
                target,
                cwd.display()
            );
        }
        candidate
    };

    let txt = std::fs::read_to_string(&path)?;
    let entries: Vec<SessionEntry> = txt
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();

    let html = flamegraph::render(&session_id, &entries);
    println!("{html}");
    Ok(())
}

/// `pi --refresh-models` — query every provider with credentials for its
/// live model catalogue, merge into `<agent_dir>/discovered-models.json`,
/// and report per-provider success/failure.
///
/// This needs an async runtime, so it can't sit in the synchronous fast-path
/// in `bin/pi.rs`; the binary spins one up on demand.
pub async fn run_refresh_models() -> anyhow::Result<()> {
    use crate::context::{agent_dir, auth_path};
    use pi_ai::{discovered_cache_path, refresh_and_save, AuthStorage, ModelRegistry};

    // Load creds: file first, env second (env wins).
    let auth = AuthStorage::open(auth_path()).unwrap_or_else(|_| AuthStorage::in_memory());
    let env = AuthStorage::from_env();
    for (p, _) in AuthStorage::ENV_KEYS {
        if let Some(m) = env.get(p) {
            auth.set(p, m);
        }
    }
    let registry = ModelRegistry::new(auth.clone());

    let cache_path = discovered_cache_path(&agent_dir());
    let (cache, results) = refresh_and_save(&registry, &auth, &cache_path).await?;

    println!("discovered-models cache: {}", cache_path.display());
    let mut total = 0usize;
    for r in &results {
        match &r.result {
            Ok(models) => {
                total += models.len();
                println!("  ✓ {} → {} models", r.provider, models.len());
            }
            Err(e) => println!("  ✗ {} → {}", r.provider, e),
        }
    }
    println!("total: {} models across {} providers", total, cache.providers.len());
    Ok(())
}
