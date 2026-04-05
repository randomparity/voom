use anyhow::{Context, Result};
use console::style;

use crate::app;
use crate::cli::{InspectArgs, OutputFormat};
use crate::commands::history::{collect_lineage, collect_lineage_transitions};
use crate::config;
use crate::output;
use voom_domain::storage::{FileStorage, FileTransitionStorage};

/// Run the inspect command.
///
/// When the file is already in the database we return the stored data.
/// Otherwise we create a temporary `FfprobeIntrospectorPlugin` for a one-shot
/// introspection. This bypasses the kernel-registered instance intentionally:
/// inspect does not need the full plugin lifecycle (event bus, storage
/// persistence, etc.) and should work even when the kernel is not bootstrapped.
/// The ad-hoc instance respects `ffprobe_path` from config.
pub fn run(args: InspectArgs) -> Result<()> {
    let path = args
        .file
        .canonicalize()
        .with_context(|| format!("File not found: {}", args.file.display()))?;

    // First check if we have it in the database
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    match store.file_by_path(&path) {
        Ok(Some(file)) => {
            let transitions = if args.history {
                let lineage = collect_lineage(store.as_ref() as &dyn FileStorage, file.id);
                collect_lineage_transitions(store.as_ref() as &dyn FileTransitionStorage, &lineage)
            } else {
                Vec::new()
            };

            match args.format {
                OutputFormat::Json => {
                    if args.history {
                        format_inspect_json_with_history(&file, &transitions);
                    } else {
                        output::format_file_json(&file);
                    }
                }
                OutputFormat::Table => {
                    output::format_file_info(&file, args.tracks_only);
                    if args.history && !transitions.is_empty() {
                        println!();
                        println!(
                            "{}",
                            style(format!("History ({} transitions)", transitions.len())).bold()
                        );
                        let table = output::render_transitions_table(&transitions);
                        println!("{table}");
                    }
                }
                OutputFormat::Plain => println!("{}", file.path.display()),
            }
            return Ok(());
        }
        Ok(None) => {} // Not in DB — fall through to live introspection
        Err(e) => {
            tracing::warn!(error = %e, "database lookup failed, falling back to live introspection");
        }
    }

    // Not in DB — introspect live
    if !args.format.is_machine() {
        eprintln!("{}", style("File not in database, introspecting...").dim());
    }

    let mut introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    if let Some(fp) = config.ffprobe_path() {
        introspector = introspector.with_ffprobe_path(fp);
    }
    let size = std::fs::metadata(&path)?.len();

    let event = introspector
        .introspect(&path, size, None)
        .context("Introspection failed")?;

    match args.format {
        OutputFormat::Json => output::format_file_json(&event.file),
        OutputFormat::Table => output::format_file_info(&event.file, args.tracks_only),
        OutputFormat::Plain => println!("{}", event.file.path.display()),
    }

    Ok(())
}

fn format_inspect_json_with_history(
    file: &voom_domain::MediaFile,
    transitions: &[voom_domain::transition::FileTransition],
) {
    let mut file_json = serde_json::to_value(file).expect("MediaFile serialization cannot fail");

    let history: Vec<serde_json::Value> = transitions
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id.to_string(),
                "file_id": t.file_id.to_string(),
                "path": t.path.display().to_string(),
                "from_hash": t.from_hash,
                "to_hash": t.to_hash,
                "from_size": t.from_size,
                "to_size": t.to_size,
                "source": t.source.as_str(),
                "source_detail": t.source_detail,
                "plan_id": t.plan_id.map(|id| id.to_string()),
                "duration_ms": t.duration_ms,
                "actions_taken": t.actions_taken,
                "tracks_modified": t.tracks_modified,
                "outcome": t.outcome.map(|o| o.as_str()),
                "policy_name": &t.policy_name,
                "phase_name": &t.phase_name,
                "metadata_snapshot": t.metadata_snapshot.as_ref()
                    .and_then(|s| serde_json::to_value(s).ok()),
                "created_at": t.created_at.to_rfc3339(),
            })
        })
        .collect();

    file_json
        .as_object_mut()
        .expect("MediaFile serializes to an object")
        .insert("history".to_string(), serde_json::Value::Array(history));

    println!(
        "{}",
        serde_json::to_string_pretty(&file_json)
            .expect("serde_json::Value serialization cannot fail")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_nonexistent_file_returns_error() {
        let args = InspectArgs {
            file: std::path::PathBuf::from("/nonexistent/video.mkv"),
            format: OutputFormat::Table,
            tracks_only: false,
            history: false,
        };
        let result = run(args);
        assert!(result.is_err());
    }

    #[test]
    fn inspect_args_history_flag_defaults_to_false() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            inspect: InspectArgs,
        }

        let cli = Cli::parse_from(["test", "video.mkv"]);
        assert!(!cli.inspect.history);
    }

    #[test]
    fn inspect_args_history_flag_parses() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            inspect: InspectArgs,
        }

        let cli = Cli::parse_from(["test", "--history", "video.mkv"]);
        assert!(cli.inspect.history);
    }
}
