use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// VOOM — Video Orchestration Operations Manager
#[derive(Parser)]
#[command(name = "voom", version, about, long_about = None)]
pub struct Cli {
    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Discover and introspect media files
    Scan(ScanArgs),

    /// Show file metadata
    Inspect(InspectArgs),

    /// Apply policy to files
    Process(ProcessArgs),

    /// Policy management
    #[command(subcommand)]
    Policy(PolicyCommands),

    /// Plugin management
    #[command(subcommand)]
    Plugin(PluginCommands),

    /// Job management
    #[command(subcommand)]
    Jobs(JobsCommands),

    /// Generate a report of the media library
    Report(ReportArgs),

    /// System health check
    Doctor,

    /// Start the web server
    Serve(ServeArgs),

    /// Database maintenance
    #[command(subcommand)]
    Db(DbCommands),

    /// Configuration management
    #[command(subcommand)]
    Config(ConfigCommands),

    /// External tool management
    #[command(subcommand)]
    Tools(ToolsCommands),

    /// Show file change history
    History(HistoryArgs),

    /// Backup management
    #[command(subcommand)]
    Backup(BackupCommands),

    /// First-time setup
    Init,

    /// Show library and daemon status
    Status,

    /// Generate shell completions
    Completions(CompletionsArgs),
}

// === Scan ===

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Directory to scan for media files
    pub path: PathBuf,

    /// Recurse into subdirectories
    #[arg(short, long, default_value_t = true)]
    pub recursive: bool,

    /// Number of parallel workers for hashing
    #[arg(short, long, default_value_t = 0)]
    pub workers: usize,

    /// Skip content hashing
    #[arg(long)]
    pub no_hash: bool,

    /// Show full file table after scan
    #[arg(long)]
    pub table: bool,
}

// === Inspect ===

#[derive(clap::Args)]
pub struct InspectArgs {
    /// Media file to inspect
    pub file: PathBuf,

    /// Output format
    #[arg(short, long, default_value = "table")]
    pub format: OutputFormat,

    /// Show only track information
    #[arg(long)]
    pub tracks_only: bool,
}

// === Process ===

#[derive(clap::Args)]
pub struct ProcessArgs {
    /// Directory or file to process
    pub path: PathBuf,

    /// Policy file (.voom) to apply
    #[arg(short, long)]
    pub policy: PathBuf,

    /// Show what would be done without making changes
    #[arg(long)]
    pub dry_run: bool,

    /// Error handling strategy
    #[arg(long, default_value = "fail")]
    pub on_error: ErrorHandling,

    /// Number of parallel workers
    #[arg(short, long, default_value_t = 0)]
    pub workers: usize,

    /// Require approval for each file
    #[arg(long)]
    pub approve: bool,

    /// Skip creating backups before modifications
    #[arg(long)]
    pub no_backup: bool,

    /// Re-attempt introspection on previously failed files
    #[arg(long)]
    pub force_rescan: bool,

    /// Tag files whose output is larger than the original (post-execution)
    #[arg(long)]
    pub flag_size_increase: bool,
}

// === Policy ===

#[derive(Subcommand)]
pub enum PolicyCommands {
    /// List loaded policies
    List,
    /// Validate a policy file
    Validate {
        /// Policy file to validate
        file: PathBuf,
    },
    /// Show compiled policy details
    Show {
        /// Policy file to show
        file: PathBuf,
    },
    /// Auto-format a policy file in place
    Format {
        /// Policy file to format
        file: PathBuf,
    },
}

// === Plugin ===

#[derive(Subcommand)]
pub enum PluginCommands {
    /// List registered plugins
    List,
    /// Show detailed info about a plugin
    Info {
        /// Plugin name
        name: String,
    },
    /// Enable a plugin
    Enable {
        /// Plugin name
        name: String,
    },
    /// Disable a plugin
    Disable {
        /// Plugin name
        name: String,
    },
    /// Install a WASM plugin
    Install {
        /// Path to .wasm file
        path: PathBuf,
    },
}

// === Jobs ===

#[derive(Subcommand)]
pub enum JobsCommands {
    /// List jobs
    List {
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
        /// Maximum number of jobs to display
        #[arg(short = 'n', long, default_value = "50")]
        limit: u32,
    },
    /// Show job details
    Status {
        /// Job ID
        id: String,
    },
    /// Cancel a running job
    Cancel {
        /// Job ID
        id: String,
    },
}

// === Report ===

#[derive(clap::Args)]
pub struct ReportArgs {
    /// Output format
    #[arg(short, long, default_value = "table")]
    pub format: OutputFormat,

    /// Show only files with safeguard violations (processing issues)
    #[arg(long)]
    pub issues: bool,

    /// Show per-phase plan processing summary
    #[arg(long)]
    pub plans: bool,
}

// === Serve ===

#[derive(clap::Args)]
pub struct ServeArgs {
    /// Port to listen on
    #[arg(short, long, default_value_t = 8080)]
    pub port: u16,

    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
}

// === Db ===

#[derive(Subcommand)]
pub enum DbCommands {
    /// Remove entries for files that no longer exist
    Prune,
    /// Compact the database
    Vacuum,
    /// Reset the database (destructive!)
    Reset,
    /// List files that failed introspection
    ListBad {
        /// Filter by path prefix
        #[arg(long)]
        path: Option<String>,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove bad file DB entries without deleting files from disk
    PurgeBad,
    /// Delete bad files from disk and remove their DB entries
    CleanBad {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

// === Config ===

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Open configuration in $EDITOR
    Edit,
}

// === Tools ===

#[derive(Subcommand)]
pub enum ToolsCommands {
    /// List detected external tools
    List {
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
    /// Show detailed info about a tool
    Info {
        /// Tool name (e.g. ffmpeg, mkvmerge)
        name: String,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
}

// === History ===

#[derive(clap::Args)]
pub struct HistoryArgs {
    /// Media file to show history for
    pub file: PathBuf,

    /// Output format
    #[arg(short, long, default_value = "table")]
    pub format: OutputFormat,
}

// === Backup ===

#[derive(Subcommand)]
pub enum BackupCommands {
    /// List backup files
    List {
        /// Directory to scan for backups
        path: PathBuf,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
    /// Restore a file from its backup
    Restore {
        /// Path to the .vbak backup file
        backup_path: PathBuf,
    },
    /// Remove all backup files
    Cleanup {
        /// Directory to scan for backups
        path: PathBuf,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

// === Completions ===

#[derive(clap::Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    pub shell: clap_complete::Shell,
}

// === Shared enums ===

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ErrorHandling {
    /// Continue processing remaining files after an error.
    Continue,
    /// Stop all processing on the first error.
    Fail,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Helper to parse CLI args from a string slice.
    fn parse(args: &[&str]) -> Cli {
        Cli::parse_from(args)
    }

    fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    // ── Top-level flags ──────────────────────────────────────

    #[test]
    fn test_verbose_default_is_zero() {
        let cli = parse(&["voom", "doctor"]);
        assert_eq!(cli.verbose, 0);
    }

    #[test]
    fn test_verbose_short_flag_counts() {
        let cli = parse(&["voom", "-v", "doctor"]);
        assert_eq!(cli.verbose, 1);
        let cli = parse(&["voom", "-vv", "doctor"]);
        assert_eq!(cli.verbose, 2);
        let cli = parse(&["voom", "-vvv", "doctor"]);
        assert_eq!(cli.verbose, 3);
    }

    #[test]
    fn test_verbose_long_flag() {
        let cli = parse(&["voom", "--verbose", "--verbose", "doctor"]);
        assert_eq!(cli.verbose, 2);
    }

    // ── Scan ─────────────────────────────────────────────────

    #[test]
    fn test_scan_required_path() {
        assert!(try_parse(&["voom", "scan"]).is_err());
    }

    #[test]
    fn test_scan_defaults() {
        let cli = parse(&["voom", "scan", "/media"]);
        match cli.command {
            Commands::Scan(args) => {
                assert_eq!(args.path, PathBuf::from("/media"));
                assert!(args.recursive);
                assert_eq!(args.workers, 0);
                assert!(!args.no_hash);
                assert!(!args.table);
            }
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn test_scan_flags() {
        let cli = parse(&[
            "voom",
            "scan",
            "/media",
            "--no-hash",
            "--workers",
            "4",
            "--table",
        ]);
        match cli.command {
            Commands::Scan(args) => {
                assert!(args.no_hash);
                assert_eq!(args.workers, 4);
                assert!(args.table);
            }
            _ => panic!("expected Scan"),
        }
    }

    // ── Inspect ──────────────────────────────────────────────

    #[test]
    fn test_inspect_required_file() {
        assert!(try_parse(&["voom", "inspect"]).is_err());
    }

    #[test]
    fn test_inspect_defaults() {
        let cli = parse(&["voom", "inspect", "movie.mkv"]);
        match cli.command {
            Commands::Inspect(args) => {
                assert_eq!(args.file, PathBuf::from("movie.mkv"));
                assert!(matches!(args.format, OutputFormat::Table));
                assert!(!args.tracks_only);
            }
            _ => panic!("expected Inspect"),
        }
    }

    #[test]
    fn test_inspect_json_format() {
        let cli = parse(&["voom", "inspect", "movie.mkv", "--format", "json"]);
        match cli.command {
            Commands::Inspect(args) => assert!(matches!(args.format, OutputFormat::Json)),
            _ => panic!("expected Inspect"),
        }
    }

    #[test]
    fn test_inspect_tracks_only() {
        let cli = parse(&["voom", "inspect", "movie.mkv", "--tracks-only"]);
        match cli.command {
            Commands::Inspect(args) => assert!(args.tracks_only),
            _ => panic!("expected Inspect"),
        }
    }

    // ── Process ──────────────────────────────────────────────

    #[test]
    fn test_process_requires_path_and_policy() {
        assert!(try_parse(&["voom", "process"]).is_err());
        assert!(try_parse(&["voom", "process", "/media"]).is_err());
    }

    #[test]
    fn test_process_defaults() {
        let cli = parse(&["voom", "process", "/media", "--policy", "my.voom"]);
        match cli.command {
            Commands::Process(args) => {
                assert_eq!(args.path, PathBuf::from("/media"));
                assert_eq!(args.policy, PathBuf::from("my.voom"));
                assert!(!args.dry_run);
                assert!(matches!(args.on_error, ErrorHandling::Fail));
                assert_eq!(args.workers, 0);
                assert!(!args.approve);
                assert!(!args.no_backup);
            }
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_process_all_flags() {
        let cli = parse(&[
            "voom",
            "process",
            "/media",
            "--policy",
            "p.voom",
            "--dry-run",
            "--on-error",
            "continue",
            "--workers",
            "8",
            "--approve",
            "--no-backup",
        ]);
        match cli.command {
            Commands::Process(args) => {
                assert!(args.dry_run);
                assert!(matches!(args.on_error, ErrorHandling::Continue));
                assert_eq!(args.workers, 8);
                assert!(args.approve);
                assert!(args.no_backup);
            }
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_process_on_error_continue() {
        let cli = parse(&[
            "voom",
            "process",
            "/media",
            "--policy",
            "p.voom",
            "--on-error",
            "continue",
        ]);
        match cli.command {
            Commands::Process(args) => assert!(matches!(args.on_error, ErrorHandling::Continue)),
            _ => panic!("expected Process"),
        }
    }

    // ── Policy subcommands ───────────────────────────────────

    #[test]
    fn test_policy_list() {
        let cli = parse(&["voom", "policy", "list"]);
        assert!(matches!(
            cli.command,
            Commands::Policy(PolicyCommands::List)
        ));
    }

    #[test]
    fn test_policy_validate_requires_file() {
        assert!(try_parse(&["voom", "policy", "validate"]).is_err());
    }

    #[test]
    fn test_policy_validate() {
        let cli = parse(&["voom", "policy", "validate", "my.voom"]);
        match cli.command {
            Commands::Policy(PolicyCommands::Validate { file }) => {
                assert_eq!(file, PathBuf::from("my.voom"));
            }
            _ => panic!("expected Policy Validate"),
        }
    }

    #[test]
    fn test_policy_show() {
        let cli = parse(&["voom", "policy", "show", "my.voom"]);
        match cli.command {
            Commands::Policy(PolicyCommands::Show { file }) => {
                assert_eq!(file, PathBuf::from("my.voom"));
            }
            _ => panic!("expected Policy Show"),
        }
    }

    #[test]
    fn test_policy_format() {
        let cli = parse(&["voom", "policy", "format", "my.voom"]);
        match cli.command {
            Commands::Policy(PolicyCommands::Format { file }) => {
                assert_eq!(file, PathBuf::from("my.voom"));
            }
            _ => panic!("expected Policy Format"),
        }
    }

    // ── Plugin subcommands ───────────────────────────────────

    #[test]
    fn test_plugin_list() {
        let cli = parse(&["voom", "plugin", "list"]);
        assert!(matches!(
            cli.command,
            Commands::Plugin(PluginCommands::List)
        ));
    }

    #[test]
    fn test_plugin_info() {
        let cli = parse(&["voom", "plugin", "info", "ffmpeg-executor"]);
        match cli.command {
            Commands::Plugin(PluginCommands::Info { name }) => {
                assert_eq!(name, "ffmpeg-executor");
            }
            _ => panic!("expected Plugin Info"),
        }
    }

    #[test]
    fn test_plugin_enable() {
        let cli = parse(&["voom", "plugin", "enable", "sqlite-store"]);
        match cli.command {
            Commands::Plugin(PluginCommands::Enable { name }) => {
                assert_eq!(name, "sqlite-store");
            }
            _ => panic!("expected Plugin Enable"),
        }
    }

    #[test]
    fn test_plugin_disable() {
        let cli = parse(&["voom", "plugin", "disable", "web-server"]);
        match cli.command {
            Commands::Plugin(PluginCommands::Disable { name }) => {
                assert_eq!(name, "web-server");
            }
            _ => panic!("expected Plugin Disable"),
        }
    }

    #[test]
    fn test_plugin_install() {
        let cli = parse(&["voom", "plugin", "install", "/tmp/my-plugin.wasm"]);
        match cli.command {
            Commands::Plugin(PluginCommands::Install { path }) => {
                assert_eq!(path, PathBuf::from("/tmp/my-plugin.wasm"));
            }
            _ => panic!("expected Plugin Install"),
        }
    }

    // ── Jobs subcommands ─────────────────────────────────────

    #[test]
    fn test_jobs_list_no_filter() {
        let cli = parse(&["voom", "jobs", "list"]);
        match cli.command {
            Commands::Jobs(JobsCommands::List { status, limit }) => {
                assert!(status.is_none());
                assert_eq!(limit, 50);
            }
            _ => panic!("expected Jobs List"),
        }
    }

    #[test]
    fn test_jobs_list_with_status_filter() {
        let cli = parse(&["voom", "jobs", "list", "--status", "running"]);
        match cli.command {
            Commands::Jobs(JobsCommands::List { status, .. }) => {
                assert_eq!(status.as_deref(), Some("running"));
            }
            _ => panic!("expected Jobs List"),
        }
    }

    #[test]
    fn test_jobs_status() {
        let cli = parse(&["voom", "jobs", "status", "abc-123"]);
        match cli.command {
            Commands::Jobs(JobsCommands::Status { id }) => assert_eq!(id, "abc-123"),
            _ => panic!("expected Jobs Status"),
        }
    }

    #[test]
    fn test_jobs_cancel() {
        let cli = parse(&["voom", "jobs", "cancel", "def-456"]);
        match cli.command {
            Commands::Jobs(JobsCommands::Cancel { id }) => assert_eq!(id, "def-456"),
            _ => panic!("expected Jobs Cancel"),
        }
    }

    // ── Report ───────────────────────────────────────────────

    #[test]
    fn test_report_default_format() {
        let cli = parse(&["voom", "report"]);
        match cli.command {
            Commands::Report(args) => assert!(matches!(args.format, OutputFormat::Table)),
            _ => panic!("expected Report"),
        }
    }

    #[test]
    fn test_report_json_format() {
        let cli = parse(&["voom", "report", "--format", "json"]);
        match cli.command {
            Commands::Report(args) => assert!(matches!(args.format, OutputFormat::Json)),
            _ => panic!("expected Report"),
        }
    }

    // ── Serve ────────────────────────────────────────────────

    #[test]
    fn test_serve_defaults() {
        let cli = parse(&["voom", "serve"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.port, 8080);
                assert_eq!(args.host, "127.0.0.1");
            }
            _ => panic!("expected Serve"),
        }
    }

    #[test]
    fn test_serve_custom_port_and_host() {
        let cli = parse(&["voom", "serve", "--port", "3000", "--host", "0.0.0.0"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.port, 3000);
                assert_eq!(args.host, "0.0.0.0");
            }
            _ => panic!("expected Serve"),
        }
    }

    // ── Db subcommands ───────────────────────────────────────

    #[test]
    fn test_db_prune() {
        let cli = parse(&["voom", "db", "prune"]);
        assert!(matches!(cli.command, Commands::Db(DbCommands::Prune)));
    }

    #[test]
    fn test_db_vacuum() {
        let cli = parse(&["voom", "db", "vacuum"]);
        assert!(matches!(cli.command, Commands::Db(DbCommands::Vacuum)));
    }

    #[test]
    fn test_db_reset() {
        let cli = parse(&["voom", "db", "reset"]);
        assert!(matches!(cli.command, Commands::Db(DbCommands::Reset)));
    }

    #[test]
    fn test_db_list_bad() {
        let cli = parse(&["voom", "db", "list-bad"]);
        assert!(matches!(
            cli.command,
            Commands::Db(DbCommands::ListBad { .. })
        ));
    }

    #[test]
    fn test_db_list_bad_with_path() {
        let cli = parse(&["voom", "db", "list-bad", "--path", "/media"]);
        match cli.command {
            Commands::Db(DbCommands::ListBad { path, .. }) => {
                assert_eq!(path, Some("/media".to_string()));
            }
            _ => panic!("expected ListBad"),
        }
    }

    #[test]
    fn test_db_purge_bad() {
        let cli = parse(&["voom", "db", "purge-bad"]);
        assert!(matches!(cli.command, Commands::Db(DbCommands::PurgeBad)));
    }

    #[test]
    fn test_db_clean_bad() {
        let cli = parse(&["voom", "db", "clean-bad", "--yes"]);
        match cli.command {
            Commands::Db(DbCommands::CleanBad { yes }) => {
                assert!(yes);
            }
            _ => panic!("expected CleanBad"),
        }
    }

    #[test]
    fn test_process_force_rescan() {
        let cli = parse(&[
            "voom",
            "process",
            "/tmp",
            "--policy",
            "test.voom",
            "--force-rescan",
        ]);
        match cli.command {
            Commands::Process(args) => {
                assert!(args.force_rescan);
            }
            _ => panic!("expected Process"),
        }
    }

    // ── Config subcommands ───────────────────────────────────

    #[test]
    fn test_config_show() {
        let cli = parse(&["voom", "config", "show"]);
        assert!(matches!(
            cli.command,
            Commands::Config(ConfigCommands::Show)
        ));
    }

    #[test]
    fn test_config_edit() {
        let cli = parse(&["voom", "config", "edit"]);
        assert!(matches!(
            cli.command,
            Commands::Config(ConfigCommands::Edit)
        ));
    }

    // ── Completions ──────────────────────────────────────────

    #[test]
    fn test_completions_bash() {
        let cli = parse(&["voom", "completions", "bash"]);
        match cli.command {
            Commands::Completions(args) => {
                assert_eq!(args.shell, clap_complete::Shell::Bash);
            }
            _ => panic!("expected Completions"),
        }
    }

    #[test]
    fn test_completions_zsh() {
        let cli = parse(&["voom", "completions", "zsh"]);
        match cli.command {
            Commands::Completions(args) => {
                assert_eq!(args.shell, clap_complete::Shell::Zsh);
            }
            _ => panic!("expected Completions"),
        }
    }

    #[test]
    fn test_completions_fish() {
        let cli = parse(&["voom", "completions", "fish"]);
        match cli.command {
            Commands::Completions(args) => {
                assert_eq!(args.shell, clap_complete::Shell::Fish);
            }
            _ => panic!("expected Completions"),
        }
    }

    #[test]
    fn test_completions_invalid_shell_rejected() {
        assert!(try_parse(&["voom", "completions", "nushell"]).is_err());
    }

    // ── No-arg subcommands ───────────────────────────────────

    #[test]
    fn test_doctor_subcommand() {
        let cli = parse(&["voom", "doctor"]);
        assert!(matches!(cli.command, Commands::Doctor));
    }

    #[test]
    fn test_init_subcommand() {
        let cli = parse(&["voom", "init"]);
        assert!(matches!(cli.command, Commands::Init));
    }

    #[test]
    fn test_status_subcommand() {
        let cli = parse(&["voom", "status"]);
        assert!(matches!(cli.command, Commands::Status));
    }

    // ── Invalid input ────────────────────────────────────────

    #[test]
    fn test_no_subcommand_is_error() {
        assert!(try_parse(&["voom"]).is_err());
    }

    #[test]
    fn test_unknown_subcommand_is_error() {
        assert!(try_parse(&["voom", "foobar"]).is_err());
    }

    #[test]
    fn test_invalid_output_format_rejected() {
        assert!(try_parse(&["voom", "inspect", "f.mkv", "--format", "xml"]).is_err());
    }

    #[test]
    fn test_invalid_on_error_rejected() {
        assert!(try_parse(&[
            "voom",
            "process",
            "/m",
            "--policy",
            "p",
            "--on-error",
            "retry"
        ])
        .is_err());
    }

    // ── Tools subcommands ─────────────────────────────────────

    #[test]
    fn test_tools_list() {
        let cli = parse(&["voom", "tools", "list"]);
        assert!(matches!(
            cli.command,
            Commands::Tools(ToolsCommands::List { .. })
        ));
    }

    #[test]
    fn test_tools_list_json() {
        let cli = parse(&["voom", "tools", "list", "--format", "json"]);
        match cli.command {
            Commands::Tools(ToolsCommands::List { format }) => {
                assert!(matches!(format, OutputFormat::Json));
            }
            _ => panic!("expected Tools List"),
        }
    }

    #[test]
    fn test_tools_info() {
        let cli = parse(&["voom", "tools", "info", "ffmpeg"]);
        match cli.command {
            Commands::Tools(ToolsCommands::Info { name, .. }) => {
                assert_eq!(name, "ffmpeg");
            }
            _ => panic!("expected Tools Info"),
        }
    }

    #[test]
    fn test_tools_info_requires_name() {
        assert!(try_parse(&["voom", "tools", "info"]).is_err());
    }

    // ── History ──────────────────────────────────────────────

    #[test]
    fn test_history_requires_file() {
        assert!(try_parse(&["voom", "history"]).is_err());
    }

    #[test]
    fn test_history_defaults() {
        let cli = parse(&["voom", "history", "/media/movie.mkv"]);
        match cli.command {
            Commands::History(args) => {
                assert_eq!(args.file, PathBuf::from("/media/movie.mkv"));
                assert!(matches!(args.format, OutputFormat::Table));
            }
            _ => panic!("expected History"),
        }
    }

    #[test]
    fn test_history_json_format() {
        let cli = parse(&["voom", "history", "f.mkv", "--format", "json"]);
        match cli.command {
            Commands::History(args) => assert!(matches!(args.format, OutputFormat::Json)),
            _ => panic!("expected History"),
        }
    }

    // ── Backup subcommands ──────────────────────────────────

    #[test]
    fn test_backup_list() {
        let cli = parse(&["voom", "backup", "list", "/media"]);
        match cli.command {
            Commands::Backup(BackupCommands::List { path, .. }) => {
                assert_eq!(path, PathBuf::from("/media"));
            }
            _ => panic!("expected Backup List"),
        }
    }

    #[test]
    fn test_backup_list_requires_path() {
        assert!(try_parse(&["voom", "backup", "list"]).is_err());
    }

    #[test]
    fn test_backup_restore() {
        let cli = parse(&["voom", "backup", "restore", "/path/to/file.vbak"]);
        match cli.command {
            Commands::Backup(BackupCommands::Restore { backup_path }) => {
                assert_eq!(backup_path, PathBuf::from("/path/to/file.vbak"));
            }
            _ => panic!("expected Backup Restore"),
        }
    }

    #[test]
    fn test_backup_cleanup() {
        let cli = parse(&["voom", "backup", "cleanup", "/media", "--yes"]);
        match cli.command {
            Commands::Backup(BackupCommands::Cleanup { path, yes }) => {
                assert_eq!(path, PathBuf::from("/media"));
                assert!(yes);
            }
            _ => panic!("expected Backup Cleanup"),
        }
    }

    #[test]
    fn test_backup_cleanup_requires_path() {
        assert!(try_parse(&["voom", "backup", "cleanup"]).is_err());
    }

    // ── Verbose flag is global (works after subcommand) ──────

    #[test]
    fn test_verbose_after_subcommand() {
        let cli = parse(&["voom", "doctor", "-vv"]);
        assert_eq!(cli.verbose, 2);
    }
}
