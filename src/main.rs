mod cli;
mod daemon;
mod ipc;
mod ipc_client;
mod kakoune;
mod prompt;
mod status;
mod transcript;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Daemon(options) => daemon::run(options).await,
        cli::Command::Prompt(options) => prompt::run(options).await,
        cli::Command::Status(options) => status::run_status(options).await,
        cli::Command::Shutdown(options) => status::run_shutdown(options).await,
    }
}
