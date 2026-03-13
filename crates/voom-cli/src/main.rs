use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod app;
mod cli;
mod commands;
mod output;

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

    match cli.command {
        Commands::Scan(args) => commands::scan::run(args).await,
        Commands::Inspect(args) => commands::inspect::run(args).await,
        Commands::Process(args) => commands::process::run(args).await,
        Commands::Policy(sub) => commands::policy::run(sub).await,
        Commands::Plugin(sub) => commands::plugin::run(sub).await,
        Commands::Jobs(sub) => commands::jobs::run(sub).await,
        Commands::Report(args) => commands::report::run(args).await,
        Commands::Doctor => commands::doctor::run().await,
        Commands::Serve(args) => commands::serve::run(args).await,
        Commands::Db(sub) => commands::db::run(sub).await,
        Commands::Config(sub) => commands::config::run(sub).await,
        Commands::Init => commands::init::run().await,
        Commands::Status => commands::status::run().await,
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
    fn verbosity_mapping() {
        assert_eq!(verbosity_filter(0), "warn");
        assert_eq!(verbosity_filter(1), "info");
        assert_eq!(verbosity_filter(2), "debug");
        assert_eq!(verbosity_filter(3), "trace");
        assert_eq!(verbosity_filter(255), "trace");
    }

    #[test]
    fn cli_verify_command() {
        // clap provides a debug_assert that validates the CLI definition
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
