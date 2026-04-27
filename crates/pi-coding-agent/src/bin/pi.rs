use clap::Parser;
use pi_coding_agent::{cli::Cli, cmd, modes, startup};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

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

    let startup = startup::assemble(cli.clone()).await?;

    match cli.effective_mode() {
        pi_coding_agent::cli::Mode::Print => modes::print::run(startup).await,
        pi_coding_agent::cli::Mode::Json => modes::json::run(startup).await,
        pi_coding_agent::cli::Mode::Rpc => modes::rpc::run(startup).await,
        pi_coding_agent::cli::Mode::Interactive => modes::interactive::run(startup).await,
    }
}
