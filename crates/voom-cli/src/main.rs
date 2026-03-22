use anyhow::Result;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod app;
mod cli;
mod commands;
mod config;
mod introspect;
mod output;
mod stats;
mod tools;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up tracing based on verbosity
    let filter = verbosity_filter(cli.verbose);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter)),
        )
        .with_target(false)
        .init();

    // Install CTRL-C handler: first press cancels gracefully, second force-exits.
    let token = CancellationToken::new();
    let token_bg = token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\nInterrupted. Finishing in-flight work... (press Ctrl-C again to force quit)");
        token_bg.cancel();
        tokio::signal::ctrl_c().await.ok();
        std::process::exit(130);
    });

    match cli.command {
        Commands::Scan(args) => commands::scan::run(args, token).await,
        Commands::Inspect(args) => commands::inspect::run(args).await,
        Commands::Process(args) => commands::process::run(args, token).await,
        Commands::Policy(sub) => commands::policy::run(sub).await,
        Commands::Plugin(sub) => commands::plugin::run(sub),
        Commands::Jobs(sub) => commands::jobs::run(sub),
        Commands::Report(args) => commands::report::run(args),
        Commands::Doctor => commands::doctor::run(),
        Commands::Serve(args) => commands::serve::run(args, token).await,
        Commands::Db(sub) => commands::db::run(sub).await,
        Commands::Config(sub) => commands::config::run(sub),
        Commands::Init => commands::init::run(),
        Commands::Status => commands::status::run(),
        Commands::Completions(args) => commands::completions::run(args),
    }
}

/// Map verbosity count to tracing filter string.
fn verbosity_filter(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verbosity_mapping() {
        assert_eq!(verbosity_filter(0), "warn");
        assert_eq!(verbosity_filter(1), "info");
        assert_eq!(verbosity_filter(2), "debug");
        assert_eq!(verbosity_filter(3), "trace");
        assert_eq!(verbosity_filter(255), "trace");
    }

    #[test]
    fn test_cli_verify_command() {
        // clap provides a debug_assert that validates the CLI definition
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
