use anyhow::Result;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod app;
mod capability_collector;
mod cli;
mod commands;
mod config;
mod introspect;
mod output;
mod policy_map;
mod progress;
mod stats;
mod tools;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up tracing based on verbosity
    let filter = verbosity_filter(cli.verbose);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
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

    // Fire-and-forget: cleanup runs on the blocking pool to avoid
    // blocking the tokio runtime with filesystem I/O at startup.
    tokio::task::spawn_blocking(cleanup_wasm_temp_files);

    // Compute effective quiet: explicit --quiet OR machine-readable format
    let quiet = cli.quiet
        || matches!(&cli.command, Commands::Scan(args) if args.format.is_some_and(|f| f.is_machine()));

    let global_yes = cli.yes;

    match cli.command {
        Commands::Scan(args) => commands::scan::run(args, quiet, token).await,
        Commands::Inspect(args) => commands::inspect::run(args),
        Commands::Process(args) => commands::process::run(args, quiet, token).await,
        Commands::Policy(sub) => commands::policy::run(sub),
        Commands::Plugin(sub) => commands::plugin::run(sub),
        Commands::Jobs(sub) => commands::jobs::run(sub, global_yes),
        Commands::Report(args) => commands::report::run(args),
        Commands::Files(sub) => commands::files::run(sub, global_yes),
        Commands::Plans(sub) => commands::plans::run(sub),
        Commands::Events(args) => commands::events::run(args, token).await,
        Commands::Health(sub) => commands::health::run(sub),
        Commands::Doctor => commands::health::check(),
        Commands::Serve(args) => commands::serve::run(args, token).await,
        Commands::Db(sub) => commands::db::run(sub, global_yes).await,
        Commands::Config(sub) => commands::config::run(sub),
        Commands::Tools(sub) => commands::tools::run(sub),
        Commands::History(args) => commands::history::run(args),
        Commands::Backup(sub) => commands::backup::run(sub, global_yes),
        Commands::Init => commands::init::run(),
        Commands::Status => commands::status::run(),
        Commands::Completions(args) => commands::completions::run(args),
    }
}

/// Remove orphaned WASM plugin temp files older than 1 hour.
fn cleanup_wasm_temp_files() {
    cleanup_wasm_temp_files_in(std::path::Path::new("/tmp"));
}

/// Remove orphaned WASM plugin temp files older than 1 hour from the given directory.
fn cleanup_wasm_temp_files_in(dir: &std::path::Path) {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let mut removed = 0u32;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_wasm_temp_file(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "failed to remove orphaned WASM temp file"
                );
            } else {
                removed += 1;
            }
        }
    }

    if removed > 0 {
        tracing::info!(count = removed, "cleaned up orphaned WASM temp files");
    }
}

/// Check if a filename matches WASM plugin temp file patterns.
fn is_wasm_temp_file(name: &str) -> bool {
    (name.starts_with("voom-langdet-") || name.starts_with("voom-whisper-"))
        && name.ends_with(".wav")
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

    #[test]
    fn test_is_wasm_temp_file() {
        assert!(is_wasm_temp_file("voom-langdet-abc123.wav"));
        assert!(is_wasm_temp_file("voom-whisper-xyz.wav"));
        assert!(!is_wasm_temp_file("voom-langdet-abc123.mp4"));
        assert!(!is_wasm_temp_file("voom-other-abc.wav"));
        assert!(!is_wasm_temp_file("random-file.wav"));
    }

    #[test]
    fn test_cleanup_empty_dir() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        cleanup_wasm_temp_files_in(dir.path());
    }

    #[test]
    fn test_cleanup_nonexistent_dir() {
        cleanup_wasm_temp_files_in(std::path::Path::new("/nonexistent/path"));
    }

    #[test]
    fn test_cleanup_preserves_recent_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let path = dir.path().join("voom-langdet-test.wav");
        std::fs::write(&path, b"test").expect("write failed");
        cleanup_wasm_temp_files_in(dir.path());
        assert!(path.exists(), "recent temp file should be kept");
    }

    #[test]
    fn test_cleanup_ignores_non_matching_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let path = dir.path().join("some-other-file.wav");
        std::fs::write(&path, b"test").expect("write failed");
        cleanup_wasm_temp_files_in(dir.path());
        assert!(path.exists(), "non-matching file should not be removed");
    }
}
