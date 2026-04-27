use clap::Parser;
use pi_coding_agent::{cli::Cli, cmd, modes, startup};

fn main() -> anyhow::Result<()> {
    // Argv pre-sniff for fast-path subcommands. Building clap's command tree
    // for our 30+ flags is non-trivial; for these flags we don't need any
    // values or interactions, so a manual match shaves the parse cost.
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        return cmd::run_flamegraph(target);
    }
    if let Some(target) = &cli.share {
        return cmd::run_share(target);
    }
    if let Some(spec) = &cli.policy {
        return cmd::run_policy(spec);
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
