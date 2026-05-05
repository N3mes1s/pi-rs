use clap::Parser;
use pi_coding_agent::{cli::Cli, cmd, modes, startup};

fn main() -> anyhow::Result<()> {
    // Argv pre-sniff for fast-path subcommands. Building clap's command tree
    // for our 30+ flags is non-trivial; for these flags we don't need any
    // values or interactions, so a manual match shaves the parse cost.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "router" && args[1] == "fetch-embeddings" {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        return rt.block_on(cmd::run_router_fetch_embeddings());
    }
    if args.len() == 1 {
        match args[0].as_str() {
            "--list" => return cmd::run_list(),
            "--config" => return cmd::run_config(),
            "--update" => return cmd::run_update(),
            _ => {}
        }
    }

    let cli = Cli::parse();

    // Fast paths: synchronous subcommands don't need a tokio runtime
    // and don't emit tracing events, so we skip subscriber init too.
    if let Some(spec) = &cli.install {
        return cmd::run_install(spec);
    }
    if cli.list {
        return cmd::run_list();
    }
    if cli.config_subcommand {
        return cmd::run_config();
    }
    if cli.update {
        return cmd::run_update();
    }
    if cli.refresh_models {
        // Needs an async runtime — spin one up just for this.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        return rt.block_on(cmd::run_refresh_models());
    }
    if let Some(verb) = &cli.sandbox_subcommand {
        match verb.as_str() {
            "doctor" => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let exit = rt.block_on(cmd::run_sandbox_doctor())?;
                std::process::exit(exit);
            }
            other => {
                anyhow::bail!("unknown --sandbox verb: {other}");
            }
        }
    }
    if cli.internal_evolve_tick {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return rt.block_on(cmd::run_internal_evolve_tick());
    }
    if let Some(verb) = &cli.evolve {
        return cmd::run_evolve(verb);
    }
    if let Some(target) = &cli.flamegraph {
        // RFD 0012: dispatch on --flamegraph-format. The HTML path
        // delegates to cmd::run_flamegraph (unchanged); the JSON path
        // builds a Trajectory in-place and prints it.
        let format = cli
            .flamegraph_format
            .as_deref()
            .and_then(pi_coding_agent::native::trajectory::flamegraph::Format::parse)
            .unwrap_or(pi_coding_agent::native::trajectory::flamegraph::Format::Html);
        match format {
            pi_coding_agent::native::trajectory::flamegraph::Format::Html => {
                return cmd::run_flamegraph(target);
            }
            pi_coding_agent::native::trajectory::flamegraph::Format::Json => {
                use pi_agent_core::SessionEntry;
                use pi_coding_agent::context::sessions_dir;
                use pi_coding_agent::native::trajectory::flamegraph;
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
                let trajectory = flamegraph::build_trajectory(&session_id, &entries);
                println!("{}", flamegraph::render_json(&trajectory));
                return Ok(());
            }
        }
    }
    if let Some(target) = &cli.share {
        return cmd::run_share(target);
    }
    if let Some(spec) = &cli.policy {
        return cmd::run_policy(spec);
    }
    // ---- halo fast paths (RFD 0025 M1) ----
    if cli.halo_bootstrap_agents {
        return cmd::run_halo_bootstrap_agents();
    }
    if cli.halo_status {
        return cmd::run_halo_status(cli.watch, cli.json, cli.halo_config.as_deref());
    }
    if cli.halo_add_proposal {
        return cmd::run_halo_add_proposal(
            cli.halo_title.as_deref().unwrap_or("untitled"),
            cli.halo_rationale.as_deref(),
            cli.halo_files.as_deref(),
            cli.halo_priority,
            cli.halo_est_cost,
        );
    }
    if let Some(id) = &cli.halo_drop_proposal {
        return cmd::run_halo_drop_proposal(id);
    }
    if cli.halo_pause {
        return cmd::run_halo_pause();
    }
    if cli.halo_resume {
        return cmd::run_halo_resume();
    }
    if cli.halo_stop {
        return cmd::run_halo_stop();
    }
    if cli.halo {
        let cwd = std::env::current_dir()?;
        return pi_coding_agent::halo::run::run_supervisor(
            &cwd,
            cli.halo_config.as_deref(),
            cli.halo_max_cycles,
        );
    }
    if let Some(path) = &cli.orchestrate_dry_run {
        let text = std::fs::read_to_string(path)?;
        let campaign = match pi_orchestrate::parse_campaign(&text) {
            Ok(campaign) => campaign,
            Err(err) => {
                eprintln!("error: failed to parse campaign TOML at {}", path.display());
                eprintln!("  {err}");
                std::process::exit(2);
            }
        };

        if let Err(errors) = pi_orchestrate::validate(&campaign) {
            eprintln!(
                "error: campaign validation failed for {} ({} error(s))",
                path.display(),
                errors.len()
            );
            for error in errors {
                eprintln!("  - {error}");
            }
            std::process::exit(2);
        }

        print!("{}", pi_orchestrate::format_plan(&campaign));
        return Ok(());
    }
    if let Some(path) = &cli.orchestrate {
        let text = std::fs::read_to_string(path)?;
        let campaign = match pi_orchestrate::parse_campaign(&text) {
            Ok(c) => c,
            Err(err) => {
                eprintln!("error: failed to parse campaign TOML at {}", path.display());
                eprintln!("  {err}");
                std::process::exit(2);
            }
        };
        if let Err(errors) = pi_orchestrate::validate(&campaign) {
            eprintln!(
                "error: campaign validation failed for {} ({} error(s))",
                path.display(),
                errors.len()
            );
            for error in errors {
                eprintln!("  - {error}");
            }
            std::process::exit(2);
        }
        let state_root = cli.orchestrate_state_root.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".pi")
                .join("orchestrate")
        });

        // --orchestrate-isolate: allocate a fresh worktree, run inside it,
        // then remove it (best-effort) on exit.
        let summary = if cli.orchestrate_isolate {
            // Sanitise the campaign name for use as a filesystem path
            // component: replace slashes with underscores.
            let safe_name = campaign.name.replace('/', "_");
            let uid = uuid::Uuid::new_v4().simple().to_string();
            let wt_path = std::env::temp_dir()
                .join(format!("pi-orch-{}-{}", safe_name, &uid[..8]));

            // `git worktree add --detach <path> HEAD` — seeded from the
            // current HEAD so the child sees all milestone branches.
            let add_out = std::process::Command::new("git")
                .args(["worktree", "add", "--detach"])
                .arg(&wt_path)
                .arg("HEAD")
                .output()
                .map_err(|e| anyhow::anyhow!("git worktree add: {e}"))?;
            if !add_out.status.success() {
                anyhow::bail!(
                    "git worktree add failed: {}",
                    String::from_utf8_lossy(&add_out.stderr)
                );
            }

            let mut dispatcher = pi_orchestrate::RealDispatch::default();
            // When running inside an isolated worktree, agent definitions
            // live in the original repo (gitignored/untracked files are
            // absent from the linked worktree). Lock agent lookup to the
            // git repository root so `.pi/agents/` is always reachable
            // regardless of which subdirectory the operator invoked `pi`
            // from (e.g. `cd repo/subdir && pi --orchestrate ...`).
            //
            // We resolve the toplevel via `git rev-parse --show-toplevel`
            // rather than `std::env::current_dir()` so operators who run
            // from a sub-directory of the repo still find their agents.
            let repo_root = {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "--show-toplevel"])
                    .output()
                    .map_err(|e| anyhow::anyhow!("git rev-parse --show-toplevel: {e}"))?;
                if out.status.success() {
                    std::path::PathBuf::from(
                        String::from_utf8_lossy(&out.stdout).trim().to_string(),
                    )
                } else {
                    // Not a git repo or git unavailable — fall back to cwd.
                    std::env::current_dir()?
                }
            };
            dispatcher.agent_root = Some(repo_root);
            let result = pi_orchestrate::run_with(&campaign, &state_root, &dispatcher, &wt_path)
                .map_err(|e| anyhow::anyhow!("orchestrate run failed: {e}"));

            // Best-effort cleanup: remove the worktree regardless of
            // whether the run succeeded. A failure here is non-fatal —
            // `git worktree prune` in a later invocation will reclaim it.
            let rm_out = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&wt_path)
                .output();
            if let Ok(o) = rm_out {
                if !o.status.success() {
                    eprintln!(
                        "warning: git worktree remove --force {} failed (ignored): {}",
                        wt_path.display(),
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                }
            } else if let Err(e) = rm_out {
                eprintln!(
                    "warning: could not invoke git worktree remove for {} (ignored): {e}",
                    wt_path.display()
                );
            }

            result?
        } else {
            pi_orchestrate::run(&campaign, &state_root)
                .map_err(|e| anyhow::anyhow!("orchestrate run failed: {e}"))?
        };

        println!();
        println!("=== Run summary ===");
        println!("Campaign : {}", summary.campaign);
        println!("State    : {}", summary.state_path.display());
        for outcome in &summary.outcomes {
            println!(
                "  - {} → {} (fix-loop iters: {})",
                outcome.id, outcome.final_state, outcome.fix_loop_iterations
            );
        }
        // RFD §"Exit codes":
        //   0 — every non-FAILED milestone reached MERGED
        //   2 — at least one FAILED
        //   3 — at least one BLOCKED_ON_CONFLICT or BLOCKED_ON_REVIEW_STALE
        if summary.exit_code != 0 {
            std::process::exit(summary.exit_code);
        }
        return Ok(());
    }
    if let Some(verb) = cli.stats.clone() {
        let parsed = pi_stats::cli::StatsVerb::parse(&verb)?;
        let cfg = pi_stats::cli::StatsConfig {
            port: cli.stats_port,
            ..Default::default()
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return rt.block_on(pi_stats::cli::run(parsed, cfg));
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Async path: spin up tokio only when we actually need it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let mut startup = startup::assemble(cli.clone()).await?;

        // RFD 0006: --worktree wraps the whole agent run in an
        // isolated git worktree. We swap the runtime config's cwd to
        // the worktree dir, run normally, then reconcile the result.
        let mut wt_guard: Option<pi_coding_agent::native::worktree::WorktreeGuard> = None;
        let mut wt_finish: Option<(
            std::path::PathBuf,
            std::path::PathBuf,
            pi_coding_agent::native::worktree::WorktreeBaseline,
            String,
            pi_coding_agent::native::worktree::ReconcileMode,
        )> = None;
        if cli.worktree {
            use pi_coding_agent::native::worktree as wt;
            let repo_root = wt::git::repo_root(&startup.runtime_config.cwd)
                .await
                .unwrap_or_else(|_| startup.runtime_config.cwd.clone());
            let id = cli
                .worktree_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let dir = wt::ensure(&repo_root, &id).await?;
            let baseline = wt::capture_baseline(&repo_root).await?;
            wt::apply_baseline(&dir, &baseline).await?;
            startup.runtime_config.cwd = dir.clone();
            let mode = match cli.worktree_mode.as_deref() {
                Some("patch") => wt::ReconcileMode::Patch,
                _ => wt::ReconcileMode::Branch,
            };
            wt_guard = Some(wt::WorktreeGuard::new(dir.clone()));
            wt_finish = Some((repo_root, dir, baseline, id, mode));
        }

        let agent_result = match cli.effective_mode() {
            pi_coding_agent::cli::Mode::Print => modes::print::run(startup).await,
            pi_coding_agent::cli::Mode::Json => modes::json::run(startup).await,
            pi_coding_agent::cli::Mode::Rpc => modes::rpc::run(startup).await,
            pi_coding_agent::cli::Mode::Interactive => modes::interactive::run(startup).await,
        };

        if let Some((repo_root, dir, baseline, id, mode)) = wt_finish {
            use pi_coding_agent::native::worktree as wt;
            match wt::finish(&repo_root, &dir, &baseline, &id, mode).await {
                Ok(rec) => {
                    if let Ok(s) = serde_json::to_string(&rec) {
                        println!("{s}");
                    }
                }
                Err(e) => {
                    eprintln!("worktree reconcile failed: {e}");
                }
            }
        }
        // Drop guard ⇒ cleanup.
        drop(wt_guard);
        agent_result
    })
}
