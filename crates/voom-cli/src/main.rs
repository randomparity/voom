use anyhow::Result;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod app;
mod cli;
mod commands;
mod config;
mod introspect;
mod lock;
mod output;
mod paths;
mod policy_map;
mod progress;
mod recovery;
pub mod retention;
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
        || matches!(&cli.command, Commands::Scan(args) if args.format.is_some_and(cli::OutputFormat::is_machine));

    let global_yes = cli.yes;

    // Acquire exclusive lock for mutating commands, unless --force is set.
    let _lock = if !cli.force && command_needs_lock(&cli.command) {
        let config = config::load_config()?;
        Some(lock::ProcessLock::acquire(&config.data_dir)?)
    } else {
        None
    };

    match cli.command {
        Commands::Scan(args) => commands::scan::run(args, quiet, token).await,
        Commands::Inspect(args) => commands::inspect::run(&args),
        Commands::Process(args) => commands::process::run(args, quiet, token).await,
        Commands::Policy(sub) => commands::policy::run(sub),
        Commands::Plugin(sub) => commands::plugin::run(sub),
        Commands::Jobs(sub) => commands::jobs::run(sub, global_yes),
        Commands::Report(args) => commands::report::run(&args),
        Commands::Files(sub) => commands::files::run(sub, global_yes),
        Commands::Plans(sub) => commands::plans::run(sub),
        Commands::Events(args) => commands::events::run(args, token).await,
        Commands::Health(sub) => commands::health::run(sub),
        Commands::Doctor => commands::health::check(),
        Commands::Serve(args) => commands::serve::run(args, token).await,
        Commands::Db(sub) => commands::db::run(sub, global_yes).await,
        Commands::Config(sub) => commands::config::run(sub),
        Commands::Tools(sub) => commands::tools::run(sub),
        Commands::Verify(_cmd) => {
            anyhow::bail!("voom verify is not yet implemented (Task 16)")
        }
        Commands::History(args) => commands::history::run(&args),
        Commands::Backup(sub) => commands::backup::run(sub, global_yes),
        Commands::Init => commands::init::run(),
        Commands::Completions(args) => commands::completions::run(&args),
    }
}

/// Returns true for commands that write to the database or modify files on disk.
///
/// Read-only commands (report, status, history, inspect, etc.) skip the lock.
fn command_needs_lock(command: &Commands) -> bool {
    use cli::{BackupCommands, FilesCommands, JobsCommands};
    match command {
        Commands::Scan(_) | Commands::Process(_) | Commands::Db(_) => true,
        Commands::Jobs(sub) => matches!(
            sub,
            JobsCommands::Cancel { .. } | JobsCommands::Retry { .. } | JobsCommands::Clear { .. }
        ),
        Commands::Files(sub) => matches!(sub, FilesCommands::Delete { .. }),
        Commands::Backup(sub) => matches!(
            sub,
            BackupCommands::Restore { .. } | BackupCommands::Cleanup { .. }
        ),
        _ => false,
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
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
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
    fn test_command_needs_lock_mutating_commands() {
        use clap::Parser;
        let cases = [
            vec!["voom", "scan", "/media"],
            vec!["voom", "process", "/media"],
            vec!["voom", "db", "prune"],
            vec!["voom", "jobs", "cancel", "abc"],
            vec!["voom", "jobs", "retry", "abc"],
            vec!["voom", "jobs", "clear"],
            vec!["voom", "files", "delete", "abc"],
            vec!["voom", "backup", "restore", "/tmp/f.vbak"],
            vec!["voom", "backup", "cleanup", "/tmp"],
        ];
        for args in &cases {
            let cli = Cli::parse_from(args);
            assert!(
                command_needs_lock(&cli.command),
                "expected lock for: {args:?}"
            );
        }
    }

    #[test]
    fn test_command_needs_lock_readonly_commands() {
        use clap::Parser;
        let cases = [
            vec!["voom", "doctor"],
            vec!["voom", "inspect", "f.mkv"],
            vec!["voom", "report"],
            vec!["voom", "serve"],
            vec!["voom", "jobs", "list"],
            vec!["voom", "jobs", "status", "abc"],
            vec!["voom", "files", "list"],
            vec!["voom", "files", "show", "abc"],
            vec!["voom", "backup", "list", "/tmp"],
            vec!["voom", "history", "f.mkv"],
        ];
        for args in &cases {
            let cli = Cli::parse_from(args);
            assert!(
                !command_needs_lock(&cli.command),
                "expected no lock for: {args:?}"
            );
        }
    }

    #[test]
    fn test_force_flag_default_false() {
        use clap::Parser;
        let cli = Cli::parse_from(["voom", "doctor"]);
        assert!(!cli.force);
    }

    #[test]
    fn test_force_flag_global() {
        use clap::Parser;
        let cli = Cli::parse_from(["voom", "--force", "doctor"]);
        assert!(cli.force);
        let cli = Cli::parse_from(["voom", "scan", "/media", "--force"]);
        assert!(cli.force);
    }

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
