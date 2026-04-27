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
        let startup = startup::assemble(cli.clone()).await?;
        match cli.effective_mode() {
            pi_coding_agent::cli::Mode::Print => modes::print::run(startup).await,
            pi_coding_agent::cli::Mode::Json => modes::json::run(startup).await,
            pi_coding_agent::cli::Mode::Rpc => modes::rpc::run(startup).await,
            pi_coding_agent::cli::Mode::Interactive => modes::interactive::run(startup).await,
        }
    })
}
