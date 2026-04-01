use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// VOOM — Video Orchestration Operations Manager
#[derive(Parser)]
#[command(name = "voom", version, about, long_about = None)]
pub struct Cli {
    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress progress bars and status messages
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Assume "yes" to all confirmation prompts (for automation)
    #[arg(short, long, global = true)]
    pub yes: bool,

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

    /// File queries
    #[command(subcommand)]
    Files(FilesCommands),

    /// Plan inspection
    #[command(subcommand)]
    Plans(PlansCommands),

    /// View event log
    Events(EventsArgs),

    /// System health checks and history
    #[command(subcommand)]
    Health(HealthCommands),

    /// System health check (alias for `health check`)
    #[command(hide = true)]
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
    /// Directories to scan for media files
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,

    /// Recurse into subdirectories
    #[arg(short, long, default_value_t = true)]
    pub recursive: bool,

    /// Number of parallel workers for hashing
    #[arg(short, long, default_value_t = 0)]
    pub workers: usize,

    /// Skip content hashing
    #[arg(long)]
    pub no_hash: bool,

    /// Output format (omit for summary only)
    #[arg(short, long)]
    pub format: Option<OutputFormat>,
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
    /// Directories or files to process
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,

    /// Policy file (.voom) to apply to all files
    #[arg(short, long, conflicts_with = "policy_map")]
    pub policy: Option<PathBuf>,

    /// TOML file mapping directory prefixes to policies
    #[arg(long, conflicts_with = "policy")]
    pub policy_map: Option<PathBuf>,

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

    /// Output raw plans as JSON to stdout without executing (implies --dry-run)
    #[arg(long)]
    pub plan_only: bool,

    /// Assign job priority based on file modification date
    #[arg(long)]
    pub priority_by_date: bool,
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
    /// Compare two compiled policies
    Diff {
        /// First policy file
        a: PathBuf,
        /// Second policy file
        b: PathBuf,
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
        /// Number of jobs to skip
        #[arg(long, default_value = "0")]
        offset: u32,
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
    /// Retry a failed job
    Retry {
        /// Job ID
        id: String,
    },
    /// Delete completed/failed/cancelled jobs
    Clear {
        /// Only delete jobs with this status
        #[arg(long)]
        status: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
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

    /// Show deep library statistics
    #[arg(long)]
    pub stats: bool,

    /// Show snapshot history (N most recent)
    #[arg(long)]
    pub history: Option<u32>,
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
    Reset {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
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
    /// Show database size, row counts, and fragmentation
    Stats {
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
}

// === Config ===

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Open configuration in $EDITOR
    Edit,
    /// Get a configuration value by dot-notation key
    Get {
        /// Dot-notation key (e.g. auth_token, plugins.wasm_dir, plugin.ffmpeg-executor.hw_accel)
        key: String,
    },
    /// Set a configuration value by dot-notation key
    Set {
        /// Dot-notation key (e.g. auth_token, plugin.ffmpeg-executor.hw_accel)
        key: String,
        /// Value to set (auto-detects type: bool, int, float, or string)
        value: String,
    },
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
        /// Directories to scan for backups
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
    /// Restore a file from its backup
    Restore {
        /// Path to the .vbak backup file
        backup_path: PathBuf,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Remove all backup files
    Cleanup {
        /// Directories to scan for backups
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

// === Files ===

#[derive(Subcommand)]
pub enum FilesCommands {
    /// List files with optional filters
    List {
        /// Filter by container format (e.g. mkv, mp4)
        #[arg(long)]
        container: Option<String>,
        /// Filter by codec (e.g. hevc, aac)
        #[arg(long)]
        codec: Option<String>,
        /// Filter by track language (e.g. eng, jpn)
        #[arg(long)]
        lang: Option<String>,
        /// Filter by path prefix
        #[arg(long)]
        path_prefix: Option<String>,
        /// Maximum number of files to display
        #[arg(short = 'n', long, default_value = "100")]
        limit: u32,
        /// Number of files to skip
        #[arg(long, default_value = "0")]
        offset: u32,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
    /// Show details for a single file by UUID
    Show {
        /// File UUID
        id: String,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
    /// Delete a file from the database by UUID
    Delete {
        /// File UUID
        id: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

// === Plans ===

#[derive(Subcommand)]
pub enum PlansCommands {
    /// Show plans for a file
    Show {
        /// File UUID or path
        file: String,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
}

// === Health ===

#[derive(Subcommand)]
pub enum HealthCommands {
    /// Run live system health checks
    Check,
    /// Show health check history from the database
    History {
        /// Filter by check name
        #[arg(long)]
        check: Option<String>,
        /// Show only records since this datetime
        /// (e.g. 2024-01-15 or 2024-01-15T10:30:00)
        #[arg(long)]
        since: Option<String>,
        /// Maximum number of records to display
        #[arg(short = 'n', long, default_value = "50")]
        limit: u32,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: OutputFormat,
    },
}

// === Events ===

#[derive(clap::Args)]
pub struct EventsArgs {
    /// Keep streaming new events
    #[arg(short = 'F', long)]
    pub follow: bool,

    /// Filter by event type (e.g. file.discovered, job.*)
    #[arg(long)]
    pub filter: Option<String>,

    /// Output format
    #[arg(short, long, default_value = "table")]
    pub format: OutputFormat,

    /// Maximum events to display
    #[arg(short = 'n', long, default_value = "50")]
    pub limit: u32,
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
    Plain,
}

impl OutputFormat {
    /// Returns true for formats intended for machine consumption (piping, scripting).
    pub fn is_machine(&self) -> bool {
        matches!(self, Self::Json | Self::Plain)
    }
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

    #[test]
    fn test_quiet_default_is_false() {
        let cli = parse(&["voom", "doctor"]);
        assert!(!cli.quiet);
    }

    #[test]
    fn test_quiet_short_flag() {
        let cli = parse(&["voom", "-q", "doctor"]);
        assert!(cli.quiet);
    }

    #[test]
    fn test_quiet_long_flag() {
        let cli = parse(&["voom", "--quiet", "doctor"]);
        assert!(cli.quiet);
    }

    #[test]
    fn test_quiet_after_subcommand() {
        let cli = parse(&["voom", "scan", "/media", "--quiet"]);
        assert!(cli.quiet);
    }

    #[test]
    fn test_yes_default_is_false() {
        let cli = parse(&["voom", "doctor"]);
        assert!(!cli.yes);
    }

    #[test]
    fn test_yes_short_flag() {
        let cli = parse(&["voom", "-y", "doctor"]);
        assert!(cli.yes);
    }

    #[test]
    fn test_yes_long_flag() {
        let cli = parse(&["voom", "--yes", "doctor"]);
        assert!(cli.yes);
    }

    #[test]
    fn test_yes_after_subcommand() {
        let cli = parse(&["voom", "scan", "/media", "--yes"]);
        assert!(cli.yes);
    }

    #[test]
    fn test_db_reset_yes() {
        let cli = parse(&["voom", "db", "reset", "--yes"]);
        match cli.command {
            Commands::Db(DbCommands::Reset { yes }) => assert!(yes),
            _ => panic!("expected Db Reset"),
        }
    }

    #[test]
    fn test_global_yes_with_db_reset() {
        let cli = parse(&["voom", "-y", "db", "reset"]);
        assert!(cli.yes);
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
                assert_eq!(args.paths, vec![PathBuf::from("/media")]);
                assert!(args.recursive);
                assert_eq!(args.workers, 0);
                assert!(!args.no_hash);
                assert!(args.format.is_none());
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
            "--format",
            "json",
        ]);
        match cli.command {
            Commands::Scan(args) => {
                assert!(args.no_hash);
                assert_eq!(args.workers, 4);
                assert!(matches!(args.format, Some(OutputFormat::Json)));
            }
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn test_scan_plain_format() {
        let cli = parse(&["voom", "scan", "/media", "--format", "plain"]);
        match cli.command {
            Commands::Scan(args) => assert!(matches!(args.format, Some(OutputFormat::Plain))),
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn test_scan_table_format() {
        let cli = parse(&["voom", "scan", "/media", "--format", "table"]);
        match cli.command {
            Commands::Scan(args) => assert!(matches!(args.format, Some(OutputFormat::Table))),
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
    fn test_process_requires_path() {
        assert!(try_parse(&["voom", "process"]).is_err());
    }

    #[test]
    fn test_process_no_policy_is_ok() {
        let cli = parse(&["voom", "process", "/media"]);
        match cli.command {
            Commands::Process(args) => {
                assert!(args.policy.is_none());
                assert!(args.policy_map.is_none());
            }
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_process_defaults() {
        let cli = parse(&["voom", "process", "/media", "--policy", "my.voom"]);
        match cli.command {
            Commands::Process(args) => {
                assert_eq!(args.paths, vec![PathBuf::from("/media")]);
                assert_eq!(args.policy, Some(PathBuf::from("my.voom")));
                assert!(args.policy_map.is_none());
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
    fn test_process_policy_map_flag() {
        let cli = parse(&["voom", "process", "/media", "--policy-map", "map.toml"]);
        match cli.command {
            Commands::Process(args) => {
                assert!(args.policy.is_none());
                assert_eq!(args.policy_map, Some(PathBuf::from("map.toml")));
            }
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_process_policy_and_map_conflict() {
        assert!(try_parse(&[
            "voom",
            "process",
            "/media",
            "--policy",
            "p.voom",
            "--policy-map",
            "map.toml"
        ])
        .is_err());
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

    #[test]
    fn test_policy_diff() {
        let cli = parse(&["voom", "policy", "diff", "a.voom", "b.voom"]);
        match cli.command {
            Commands::Policy(PolicyCommands::Diff { a, b }) => {
                assert_eq!(a, PathBuf::from("a.voom"));
                assert_eq!(b, PathBuf::from("b.voom"));
            }
            _ => panic!("expected Policy Diff"),
        }
    }

    #[test]
    fn test_policy_diff_requires_two_files() {
        assert!(try_parse(&["voom", "policy", "diff"]).is_err());
        assert!(try_parse(&["voom", "policy", "diff", "a.voom"]).is_err());
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
            Commands::Jobs(JobsCommands::List {
                status,
                limit,
                offset,
            }) => {
                assert!(status.is_none());
                assert_eq!(limit, 50);
                assert_eq!(offset, 0);
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
    fn test_jobs_list_with_offset() {
        let cli = parse(&["voom", "jobs", "list", "--offset", "10"]);
        match cli.command {
            Commands::Jobs(JobsCommands::List { offset, .. }) => {
                assert_eq!(offset, 10);
            }
            _ => panic!("expected Jobs List"),
        }
    }

    #[test]
    fn test_jobs_retry() {
        let cli = parse(&["voom", "jobs", "retry", "abc-123"]);
        match cli.command {
            Commands::Jobs(JobsCommands::Retry { id }) => {
                assert_eq!(id, "abc-123");
            }
            _ => panic!("expected Jobs Retry"),
        }
    }

    #[test]
    fn test_jobs_retry_requires_id() {
        assert!(try_parse(&["voom", "jobs", "retry"]).is_err());
    }

    #[test]
    fn test_jobs_clear_defaults() {
        let cli = parse(&["voom", "jobs", "clear"]);
        match cli.command {
            Commands::Jobs(JobsCommands::Clear { status, yes }) => {
                assert!(status.is_none());
                assert!(!yes);
            }
            _ => panic!("expected Jobs Clear"),
        }
    }

    #[test]
    fn test_jobs_clear_with_status_and_yes() {
        let cli = parse(&["voom", "jobs", "clear", "--status", "failed", "--yes"]);
        match cli.command {
            Commands::Jobs(JobsCommands::Clear { status, yes }) => {
                assert_eq!(status.as_deref(), Some("failed"));
                assert!(yes);
            }
            _ => panic!("expected Jobs Clear"),
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
        assert!(matches!(
            cli.command,
            Commands::Db(DbCommands::Reset { .. })
        ));
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
    fn test_db_stats() {
        let cli = parse(&["voom", "db", "stats"]);
        assert!(matches!(
            cli.command,
            Commands::Db(DbCommands::Stats { .. })
        ));
    }

    #[test]
    fn test_db_stats_json_format() {
        let cli = parse(&["voom", "db", "stats", "-f", "json"]);
        match cli.command {
            Commands::Db(DbCommands::Stats { format }) => {
                assert!(matches!(format, OutputFormat::Json));
            }
            _ => panic!("expected Stats"),
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

    #[test]
    fn test_config_get() {
        let cli = parse(&["voom", "config", "get", "auth_token"]);
        match cli.command {
            Commands::Config(ConfigCommands::Get { key }) => {
                assert_eq!(key, "auth_token");
            }
            _ => panic!("expected Config Get"),
        }
    }

    #[test]
    fn test_config_get_nested_key() {
        let cli = parse(&["voom", "config", "get", "plugin.ffmpeg-executor.hw_accel"]);
        match cli.command {
            Commands::Config(ConfigCommands::Get { key }) => {
                assert_eq!(key, "plugin.ffmpeg-executor.hw_accel");
            }
            _ => panic!("expected Config Get"),
        }
    }

    #[test]
    fn test_config_get_requires_key() {
        assert!(try_parse(&["voom", "config", "get"]).is_err());
    }

    #[test]
    fn test_config_set() {
        let cli = parse(&["voom", "config", "set", "auth_token", "mytoken"]);
        match cli.command {
            Commands::Config(ConfigCommands::Set { key, value }) => {
                assert_eq!(key, "auth_token");
                assert_eq!(value, "mytoken");
            }
            _ => panic!("expected Config Set"),
        }
    }

    #[test]
    fn test_config_set_nested_key() {
        let cli = parse(&[
            "voom",
            "config",
            "set",
            "plugin.ffmpeg-executor.hw_accel",
            "nvenc",
        ]);
        match cli.command {
            Commands::Config(ConfigCommands::Set { key, value }) => {
                assert_eq!(key, "plugin.ffmpeg-executor.hw_accel");
                assert_eq!(value, "nvenc");
            }
            _ => panic!("expected Config Set"),
        }
    }

    #[test]
    fn test_config_set_requires_key_and_value() {
        assert!(try_parse(&["voom", "config", "set"]).is_err());
        assert!(try_parse(&["voom", "config", "set", "key"]).is_err());
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

    // ── Health subcommands ────────────────────────────────────

    #[test]
    fn test_health_check() {
        let cli = parse(&["voom", "health", "check"]);
        assert!(matches!(
            cli.command,
            Commands::Health(HealthCommands::Check)
        ));
    }

    #[test]
    fn test_health_history_defaults() {
        let cli = parse(&["voom", "health", "history"]);
        match cli.command {
            Commands::Health(HealthCommands::History {
                check,
                since,
                limit,
                format,
            }) => {
                assert!(check.is_none());
                assert!(since.is_none());
                assert_eq!(limit, 50);
                assert!(matches!(format, OutputFormat::Table));
            }
            _ => panic!("expected Health History"),
        }
    }

    #[test]
    fn test_health_history_all_flags() {
        let cli = parse(&[
            "voom",
            "health",
            "history",
            "--check",
            "data_dir_exists",
            "--since",
            "2024-01-15",
            "--format",
            "json",
            "-n",
            "10",
        ]);
        match cli.command {
            Commands::Health(HealthCommands::History {
                check,
                since,
                limit,
                format,
            }) => {
                assert_eq!(check.as_deref(), Some("data_dir_exists"));
                assert_eq!(since.as_deref(), Some("2024-01-15"));
                assert_eq!(limit, 10);
                assert!(matches!(format, OutputFormat::Json));
            }
            _ => panic!("expected Health History"),
        }
    }

    #[test]
    fn test_doctor_alias_backward_compat() {
        let cli = parse(&["voom", "doctor"]);
        assert!(matches!(cli.command, Commands::Doctor));
    }

    // ── No-arg subcommands ───────────────────────────────────

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
    fn test_process_priority_by_date_flag() {
        let cli = parse(&[
            "voom",
            "process",
            "/media",
            "--policy",
            "p.voom",
            "--priority-by-date",
        ]);
        match cli.command {
            Commands::Process(args) => assert!(args.priority_by_date),
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_process_priority_by_date_default_false() {
        let cli = parse(&["voom", "process", "/media", "--policy", "p.voom"]);
        match cli.command {
            Commands::Process(args) => assert!(!args.priority_by_date),
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_output_format_is_machine() {
        assert!(!OutputFormat::Table.is_machine());
        assert!(OutputFormat::Json.is_machine());
        assert!(OutputFormat::Plain.is_machine());
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
            Commands::Backup(BackupCommands::List { paths, .. }) => {
                assert_eq!(paths, vec![PathBuf::from("/media")]);
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
            Commands::Backup(BackupCommands::Restore { backup_path, yes }) => {
                assert_eq!(backup_path, PathBuf::from("/path/to/file.vbak"));
                assert!(!yes);
            }
            _ => panic!("expected Backup Restore"),
        }
    }

    #[test]
    fn test_backup_restore_yes() {
        let cli = parse(&["voom", "backup", "restore", "/path/to/file.vbak", "--yes"]);
        match cli.command {
            Commands::Backup(BackupCommands::Restore { yes, .. }) => {
                assert!(yes);
            }
            _ => panic!("expected Backup Restore"),
        }
    }

    #[test]
    fn test_backup_cleanup() {
        let cli = parse(&["voom", "backup", "cleanup", "/media", "--yes"]);
        match cli.command {
            Commands::Backup(BackupCommands::Cleanup { paths, yes }) => {
                assert_eq!(paths, vec![PathBuf::from("/media")]);
                assert!(yes);
            }
            _ => panic!("expected Backup Cleanup"),
        }
    }

    #[test]
    fn test_backup_cleanup_requires_path() {
        assert!(try_parse(&["voom", "backup", "cleanup"]).is_err());
    }

    #[test]
    fn test_scan_multiple_paths() {
        let cli = parse(&["voom", "scan", "/movies", "/series"]);
        match cli.command {
            Commands::Scan(args) => {
                assert_eq!(
                    args.paths,
                    vec![PathBuf::from("/movies"), PathBuf::from("/series")]
                );
            }
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn test_scan_single_path_still_works() {
        let cli = parse(&["voom", "scan", "/media"]);
        match cli.command {
            Commands::Scan(args) => {
                assert_eq!(args.paths, vec![PathBuf::from("/media")]);
            }
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn test_scan_requires_at_least_one_path() {
        assert!(try_parse(&["voom", "scan"]).is_err());
    }

    #[test]
    fn test_process_multiple_paths() {
        let cli = parse(&[
            "voom", "process", "/movies", "/series", "--policy", "p.voom",
        ]);
        match cli.command {
            Commands::Process(args) => {
                assert_eq!(
                    args.paths,
                    vec![PathBuf::from("/movies"), PathBuf::from("/series")]
                );
            }
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_process_single_path_still_works() {
        let cli = parse(&["voom", "process", "/media", "--policy", "p.voom"]);
        match cli.command {
            Commands::Process(args) => {
                assert_eq!(args.paths, vec![PathBuf::from("/media")]);
            }
            _ => panic!("expected Process"),
        }
    }

    #[test]
    fn test_backup_list_multiple_paths() {
        let cli = parse(&["voom", "backup", "list", "/movies", "/series"]);
        match cli.command {
            Commands::Backup(BackupCommands::List { paths, .. }) => {
                assert_eq!(
                    paths,
                    vec![PathBuf::from("/movies"), PathBuf::from("/series")]
                );
            }
            _ => panic!("expected Backup List"),
        }
    }

    #[test]
    fn test_backup_cleanup_multiple_paths() {
        let cli = parse(&["voom", "backup", "cleanup", "/movies", "/series", "--yes"]);
        match cli.command {
            Commands::Backup(BackupCommands::Cleanup { paths, yes }) => {
                assert_eq!(
                    paths,
                    vec![PathBuf::from("/movies"), PathBuf::from("/series")]
                );
                assert!(yes);
            }
            _ => panic!("expected Backup Cleanup"),
        }
    }

    // ── Files subcommands ─────────────────────────────────────

    #[test]
    fn test_files_list_defaults() {
        let cli = parse(&["voom", "files", "list"]);
        match cli.command {
            Commands::Files(FilesCommands::List {
                container,
                codec,
                lang,
                path_prefix,
                limit,
                offset,
                format,
            }) => {
                assert!(container.is_none());
                assert!(codec.is_none());
                assert!(lang.is_none());
                assert!(path_prefix.is_none());
                assert_eq!(limit, 100);
                assert_eq!(offset, 0);
                assert!(matches!(format, OutputFormat::Table));
            }
            _ => panic!("expected Files List"),
        }
    }

    #[test]
    fn test_files_list_all_filters() {
        let cli = parse(&[
            "voom",
            "files",
            "list",
            "--container",
            "mkv",
            "--codec",
            "hevc",
            "--lang",
            "eng",
            "--path-prefix",
            "/media",
        ]);
        match cli.command {
            Commands::Files(FilesCommands::List {
                container,
                codec,
                lang,
                path_prefix,
                ..
            }) => {
                assert_eq!(container.as_deref(), Some("mkv"));
                assert_eq!(codec.as_deref(), Some("hevc"));
                assert_eq!(lang.as_deref(), Some("eng"));
                assert_eq!(path_prefix.as_deref(), Some("/media"));
            }
            _ => panic!("expected Files List"),
        }
    }

    #[test]
    fn test_files_list_json_format() {
        let cli = parse(&["voom", "files", "list", "--format", "json"]);
        match cli.command {
            Commands::Files(FilesCommands::List { format, .. }) => {
                assert!(matches!(format, OutputFormat::Json));
            }
            _ => panic!("expected Files List"),
        }
    }

    #[test]
    fn test_files_list_limit_short_flag() {
        let cli = parse(&["voom", "files", "list", "-n", "25"]);
        match cli.command {
            Commands::Files(FilesCommands::List { limit, .. }) => {
                assert_eq!(limit, 25);
            }
            _ => panic!("expected Files List"),
        }
    }

    // ── Plans subcommands ──────────────────────────────────────

    #[test]
    fn test_plans_show_requires_file() {
        assert!(try_parse(&["voom", "plans", "show"]).is_err());
    }

    #[test]
    fn test_plans_show_with_uuid() {
        let cli = parse(&[
            "voom",
            "plans",
            "show",
            "550e8400-e29b-41d4-a716-446655440000",
        ]);
        match cli.command {
            Commands::Plans(PlansCommands::Show { file, format }) => {
                assert_eq!(file, "550e8400-e29b-41d4-a716-446655440000");
                assert!(matches!(format, OutputFormat::Table));
            }
            _ => panic!("expected Plans Show"),
        }
    }

    #[test]
    fn test_plans_show_with_path() {
        let cli = parse(&["voom", "plans", "show", "/media/movie.mkv"]);
        match cli.command {
            Commands::Plans(PlansCommands::Show { file, .. }) => {
                assert_eq!(file, "/media/movie.mkv");
            }
            _ => panic!("expected Plans Show"),
        }
    }

    #[test]
    fn test_plans_show_json_format() {
        let cli = parse(&[
            "voom",
            "plans",
            "show",
            "550e8400-e29b-41d4-a716-446655440000",
            "--format",
            "json",
        ]);
        match cli.command {
            Commands::Plans(PlansCommands::Show { format, .. }) => {
                assert!(matches!(format, OutputFormat::Json));
            }
            _ => panic!("expected Plans Show"),
        }
    }

    // ── Verbose flag is global (works after subcommand) ──────

    #[test]
    fn test_verbose_after_subcommand() {
        let cli = parse(&["voom", "health", "check", "-vv"]);
        assert_eq!(cli.verbose, 2);
    }
}
