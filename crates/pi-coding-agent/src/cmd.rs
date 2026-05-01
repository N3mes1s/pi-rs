//! Top-level subcommands: install / list / config / update.
//! Used when the `--install`, `--list`, `--config`, or `--update` flags are
//! present (mirroring `pi install`, `pi list`, etc.).

use crate::context::{agent_dir, package_dir};
use crate::packages;
use pi_agent_core::{fetch_default_embeddings, validate_embedding_model};

pub fn run_install(spec: &str) -> anyhow::Result<()> {
    let dest = package_dir();
    let pkg = packages::install(spec, &dest)?;
    println!(
        "installed {} ({}) -> {}",
        pkg.name
            .is_empty()
            .then(|| spec.to_string())
            .unwrap_or(pkg.name.clone()),
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

pub async fn run_router_fetch_embeddings() -> anyhow::Result<()> {
    let path = fetch_default_embeddings().await?;
    validate_embedding_model(&path)?;
    println!("{}", path.display());
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
    let mut log_line = format!(
        "[{}] tick start\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
    );

    // Pi binary path: prefer current_exe so the subprocess stays
    // consistent across self-replacing installs.
    let pi_binary = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("pi"));
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
        "dry-run" => run_evolve_dry_run(&cwd),
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
        other => {
            anyhow::bail!("unknown --evolve verb: {other} (expected: status, off, on, dry-run)")
        }
    }
}

/// `pi --evolve dry-run` — preview what the daemon *would* do for the
/// current cwd without spending a single token. Loads past trajectories,
/// the on-disk AGENTS.md, builds evidence, picks the next mutable
/// section, and prints the prompt that would be sent to the slow model.
/// Also surfaces the cost ledger + every gate decision.
fn run_evolve_dry_run(cwd: &std::path::Path) -> anyhow::Result<()> {
    use crate::context::{agent_dir, sessions_dir, settings_paths};
    use crate::evolve::{
        build_prompt, evolve_dir, is_disabled, load_cases, should_run, AgentsMd, CostLedger,
        EvidenceItem, MutationEvidence, State, TickDecision,
    };
    use pi_agent_core::Settings;

    let _ = evolve_dir(cwd); // ensure dir exists for ledger/state
    let (global, project) = settings_paths();
    let mut settings = Settings::load(&global);
    settings.merge_project(&project);

    let agents_md_path = locate_agents_md(cwd, &agent_dir());
    let has_agents_md = agents_md_path.is_file();

    let cwd_slug = cwd.display().to_string().replace(['/', '\\', ':'], "_");
    let cases = load_cases(
        &sessions_dir(),
        &cwd_slug,
        settings.evolve.benchmark_size as usize,
    )
    .unwrap_or_default();

    let mut cost = CostLedger::load(cwd);
    let state = State::load(cwd);
    let decision = should_run(
        &settings.evolve,
        &mut cost,
        &state,
        cwd,
        cases.len() as u32,
        has_agents_md,
    );

    println!("evolve --dry-run for {}", cwd.display());
    println!("  agents_md:        {}", agents_md_path.display());
    println!("  has_agents_md:    {}", has_agents_md);
    println!("  cases_loaded:     {}", cases.len());
    println!("  ticks_run:        {}", state.ticks_run);
    println!("  spent_today:      ${:.4}", cost.today_spend());
    println!(
        "  daily_cost_cap:   ${:.4}",
        settings.evolve.daily_cost_cap_usd
    );
    println!(
        "  generations/tick: {}",
        settings.evolve.generations_per_tick
    );
    println!("  min_samples:      {}", settings.evolve.min_samples);
    println!(
        "  disabled_locally: {}",
        if is_disabled(cwd) { "yes" } else { "no" }
    );

    match &decision {
        TickDecision::Run => println!("  gate:             would RUN"),
        TickDecision::Skip(why) => println!("  gate:             would SKIP — {:?}", why),
    }

    if !has_agents_md {
        println!("\n(no AGENTS.md found — nothing to mutate)");
        return Ok(());
    }

    let baseline_text = std::fs::read_to_string(&agents_md_path)?;
    let baseline_doc = AgentsMd::parse(&baseline_text);
    let mutable: Vec<(usize, String)> = baseline_doc
        .mutable_sections()
        .map(|(i, s)| (i, s.heading.trim_end().to_string()))
        .collect();

    println!("\nMutable sections ({}):", mutable.len());
    for (i, h) in &mutable {
        println!("  [{}] {}", i, h);
    }
    if mutable.is_empty() {
        println!("(every section is pi:keep — daemon would log + exit cleanly)");
        return Ok(());
    }

    // Mirror orchestrator: target_idx = mutable_indices[gen % len]
    // for gen=0 — the *next* mutation.
    let next_target_idx = mutable[0].0;
    println!(
        "\nNext mutation target (gen 0): section [{}] {}",
        mutable[0].0, mutable[0].1
    );

    // Build evidence the same way the orchestrator does.
    let mut wins = Vec::new();
    let mut losses = Vec::new();
    for c in &cases {
        let item = EvidenceItem {
            user_request: c.user_prompt.clone(),
            verdict_reason: c
                .historical_score
                .map(|s| format!("score={s:.2}"))
                .unwrap_or_else(|| "no score".into()),
        };
        match c.historical_success {
            Some(true) => wins.push(item),
            Some(false) => losses.push(item),
            None => {}
        }
    }
    let evidence = MutationEvidence { wins, losses };
    println!(
        "  evidence:         {} win(s), {} loss(es)",
        evidence.wins.len(),
        evidence.losses.len()
    );

    let section = &baseline_doc.sections[next_target_idx];
    let prompt = build_prompt(&baseline_doc, next_target_idx, section, &evidence);

    println!("\n──── sample mutator prompt ────────────────────────────────");
    println!("{}", prompt);
    println!("──── end prompt ───────────────────────────────────────────");

    // Cost cap proximity gauge.
    let remaining = (settings.evolve.daily_cost_cap_usd - cost.today_spend()).max(0.0);
    println!("\n  budget remaining today: ${:.4}", remaining);
    println!("(no model call made; AGENTS.md untouched)");

    Ok(())
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

/// `pi --share <session-id-or-path>` — render a session as a self-
/// contained HTML transcript via [`crate::share::render_session_html`]
/// and write it to `<agent_dir>/shares/<id>.html`. Prints the file
/// path on stdout. We don't upload anywhere (pi.dev is not part of
/// this codebase); the artefact is the file you can mail or attach.
pub fn run_share(target: &str) -> anyhow::Result<()> {
    use crate::context::{agent_dir, sessions_dir};
    use crate::share::render_session_html;
    use pi_agent_core::{SessionEntry, SessionEntryKind};

    // Resolve target → a .jsonl path. Either an absolute path that
    // already exists, or a session id we look up under the per-cwd
    // sessions root (same convention as --flamegraph).
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

    // Provider/model live on the first Meta entry; fall back to
    // "unknown" if absent so we still produce a useful artefact.
    let (provider, model) = entries
        .iter()
        .find_map(|e| match &e.kind {
            SessionEntryKind::Meta {
                provider, model, ..
            } => Some((provider.clone(), model.clone())),
            _ => None,
        })
        .unwrap_or_else(|| ("unknown".into(), "unknown".into()));

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();

    let html = render_session_html(&entries, &session_id, &provider, &model);

    let shares_dir = agent_dir().join("shares");
    std::fs::create_dir_all(&shares_dir)?;
    let out_path = shares_dir.join(format!("{session_id}.html"));
    std::fs::write(&out_path, &html)?;
    println!("{}", out_path.display());
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
    println!(
        "total: {} models across {} providers",
        total,
        cache.providers.len()
    );
    Ok(())
}

/// `pi --policy <verb [arg]>` — manage `~/.pi/agent/auto-approve.json`
/// without hand-editing JSON. Honours `PI_CODING_AGENT_DIR` for tests.
///
/// Grammar of the single string argument:
///
/// ```text
/// list
/// add    <tool>:<regex>     # append regex to bash-style command_allow_regex
/// deny   <tool>:<regex>     # append regex to bash-style command_deny_regex
/// allow  <tool>:<pattern>   # set always_approve=true on the matching rule
/// remove <tool>:<entry>     # remove the first matching entry from the rule
/// ```
///
/// The `<tool>:<…>` form mirrors how upstream pi pretty-prints rules in
/// status output. We accept `bash:` for regex verbs; for `allow` / `remove`
/// any tool name is fine. Regex strings are validated with the `regex`
/// crate before saving so a typo can't poison the file.
pub fn run_policy(spec: &str) -> anyhow::Result<()> {
    use crate::auto_approve::Policy;
    use crate::context::agent_dir;

    let path = agent_dir().join("auto-approve.json");
    let mut policy: Policy = if path.exists() {
        Policy::load(&path)?
    } else {
        Policy::default_safe()
    };

    let trimmed = spec.trim();
    let (verb, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (trimmed, ""),
    };

    match verb {
        "list" => {
            print_policy(&path, &policy);
            return Ok(());
        }
        "add" | "deny" | "allow" | "remove" => {}
        other => anyhow::bail!(
            "unknown --policy verb: {other} (expected: list, add, deny, allow, remove)"
        ),
    }

    // All non-list verbs need a `<tool>:<value>` payload.
    let (tool, value) = rest.split_once(':').ok_or_else(|| {
        anyhow::anyhow!("--policy {verb}: expected `<tool>:<value>`, got `{rest}`")
    })?;
    let tool = tool.trim();
    let value = value.trim();
    if tool.is_empty() {
        anyhow::bail!("--policy {verb}: tool name is empty");
    }
    if value.is_empty() {
        anyhow::bail!("--policy {verb}: value is empty");
    }

    // Regex verbs validate the pattern up-front so a malformed input
    // never lands in the saved policy. `allow` / `remove` accept any
    // string verbatim (the user may have written `*` as a UI gesture).
    if matches!(verb, "add" | "deny") {
        if let Err(e) = regex::Regex::new(value) {
            anyhow::bail!("--policy {verb}: invalid regex `{value}`: {e}");
        }
    }

    let summary = mutate_policy(&mut policy, verb, tool, value)?;
    policy.save(&path)?;
    println!("policy: {} (saved to {})", summary, path.display());
    Ok(())
}

/// Apply one `add | deny | allow | remove` mutation in place. Returns
/// a one-liner the caller can print as confirmation. Pure on the
/// Policy struct — no I/O — so it's straightforward to unit-test.
pub fn mutate_policy(
    policy: &mut crate::auto_approve::Policy,
    verb: &str,
    tool: &str,
    value: &str,
) -> anyhow::Result<String> {
    use crate::auto_approve::policy::ToolRule;

    // Get-or-create the matching rule.
    let idx = match policy.rules.iter().position(|r| r.tool == tool) {
        Some(i) => i,
        None => {
            policy.rules.push(ToolRule {
                tool: tool.into(),
                ..Default::default()
            });
            policy.rules.len() - 1
        }
    };
    let rule = &mut policy.rules[idx];

    match verb {
        "add" => {
            if rule.command_allow_regex.iter().any(|p| p == value) {
                return Ok(format!(
                    "no-op: `{value}` already in {tool}.command_allow_regex"
                ));
            }
            rule.command_allow_regex.push(value.into());
            Ok(format!("added `{value}` to {tool}.command_allow_regex"))
        }
        "deny" => {
            if rule.command_deny_regex.iter().any(|p| p == value) {
                return Ok(format!(
                    "no-op: `{value}` already in {tool}.command_deny_regex"
                ));
            }
            rule.command_deny_regex.push(value.into());
            Ok(format!("added `{value}` to {tool}.command_deny_regex"))
        }
        "allow" => {
            rule.always_approve = true;
            Ok(format!("set {tool}.always_approve = true"))
        }
        "remove" => {
            // Search both regex lists for the first hit; we don't try to
            // be smart about which list the user meant.
            if let Some(pos) = rule.command_allow_regex.iter().position(|p| p == value) {
                rule.command_allow_regex.remove(pos);
                return Ok(format!("removed `{value}` from {tool}.command_allow_regex"));
            }
            if let Some(pos) = rule.command_deny_regex.iter().position(|p| p == value) {
                rule.command_deny_regex.remove(pos);
                return Ok(format!("removed `{value}` from {tool}.command_deny_regex"));
            }
            anyhow::bail!("--policy remove: `{value}` not found in {tool} rule")
        }
        other => anyhow::bail!("--policy: unknown verb `{other}`"),
    }
}

fn print_policy(path: &std::path::Path, policy: &crate::auto_approve::Policy) {
    println!("policy file: {}", path.display());
    println!("default_decision: {:?}", policy.default_decision);
    if policy.rules.is_empty() {
        println!("(no rules)");
        return;
    }
    println!("rules:");
    for r in &policy.rules {
        let mut flags = Vec::new();
        if r.always_approve {
            flags.push("always_approve".to_string());
        }
        if r.always_deny {
            flags.push("always_deny".to_string());
        }
        let header = if flags.is_empty() {
            format!("  {}:", r.tool)
        } else {
            format!("  {} [{}]:", r.tool, flags.join(", "))
        };
        println!("{header}");
        if !r.command_allow_regex.is_empty() {
            println!("    command_allow_regex:");
            for p in &r.command_allow_regex {
                println!("      - {p}");
            }
        }
        if !r.command_deny_regex.is_empty() {
            println!("    command_deny_regex:");
            for p in &r.command_deny_regex {
                println!("      - {p}");
            }
        }
        if !r.path_allow_globs.is_empty() {
            println!("    path_allow_globs:");
            for g in &r.path_allow_globs {
                println!("      - {g}");
            }
        }
        if !r.path_deny_globs.is_empty() {
            println!("    path_deny_globs:");
            for g in &r.path_deny_globs {
                println!("      - {g}");
            }
        }
        if let Some(parent) = &r.inherit_from {
            println!("    inherit_from: {parent}");
        }
    }
}

// -- halo (RFD 0025 M1) -----------------------------------------------------

/// `pi --halo-bootstrap-agents` — write bundled halo agents to
/// `<cwd>/.pi/agents/` if they don't already exist. Exit 0.
pub fn run_halo_bootstrap_agents() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let written = crate::halo::bootstrap_bundled_agents(&cwd)?;
    if written.is_empty() {
        println!("halo: no agent files written (all already present)");
    } else {
        println!("halo: bootstrapped {} agent file(s):", written.len());
        for p in &written {
            println!("  {}", p.display());
        }
    }
    Ok(())
}

/// `pi --halo-status` — read-only snapshot of halo supervisor state for the
/// repo containing the cwd.
pub fn run_halo_status(watch: bool, json: bool, config_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    use std::time::Duration;
    loop {
        let cwd = std::env::current_dir()?;
        let snap = crate::halo::snapshot_with_config(&cwd, config_path)?;
        if json {
            println!("{}", serde_json::to_string(&snap)?);
        } else {
            crate::halo::render_snapshot_human(&snap);
        }
        if !watch {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(5));
        if !json {
            // crude clear so successive renders don't pile up
            println!("\x1b[2J\x1b[H");
        }
    }
}

// (legacy halo status snapshot helpers removed in M1 v2 — replaced by
//  `crate::halo::snapshot_with_config` + `crate::halo::render_snapshot_human`.)

/// `pi --halo` — run the halo supervisor.
/// M2: runs `max_cycles` cycles (default 1) then exits.
pub fn run_halo_add_proposal(
    title: &str,
    rationale: Option<&str>,
    files: Option<&str>,
    priority: Option<f64>,
    est_cost: Option<f64>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let halo_dir = crate::halo::cycle::halo_dir_for_repo(&cwd)
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let cfg_path = cwd.join(".pi").join("halo.toml");
    if !cfg_path.is_file() {
        anyhow::bail!("halo.toml not found; initialise halo first");
    }
    let backlog_jsonl = halo_dir.join("backlog.jsonl");
    let id = crate::halo::proposer::generate_proposal_id();
    let files: Vec<String> = files
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    crate::halo::backlog::append_proposal_created(
        &backlog_jsonl,
        &id,
        title,
        rationale.unwrap_or(""),
        &files,
        priority.unwrap_or(0.5),
        est_cost.unwrap_or(0.0),
        "operator:cli",
    )?;
    println!("added proposal {id}");
    Ok(())
}

pub fn run_halo_drop_proposal(id: &str) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let halo_dir = crate::halo::cycle::halo_dir_for_repo(&cwd)
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let backlog_jsonl = halo_dir.join("backlog.jsonl");
    let map = crate::halo::backlog::replay(&backlog_jsonl);
    let Some(prop) = map.get(id) else {
        anyhow::bail!("error: proposal {id} unknown");
    };
    let pid = halo_dir.join("pid");
    let live = pid.is_file();
    if live && prop.status == "dispatched" {
        let cycle = crate::halo::backlog::latest_dispatched_cycle(&backlog_jsonl, id).unwrap_or(0);
        anyhow::bail!(
            "error: proposal {id} is currently dispatched in cycle {cycle}; wait for cycle terminal or run pi --halo-pause first"
        );
    }
    crate::halo::backlog::append_proposal_dropped(&backlog_jsonl, id, "operator:cli")?;
    Ok(())
}

