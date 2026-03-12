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
}

// === Config ===

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Open configuration in $EDITOR
    Edit,
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
    Skip,
    Continue,
    Fail,
}
