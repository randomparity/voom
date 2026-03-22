use anyhow::{Context, Result};
use console::style;

use crate::app;
use crate::cli::{InspectArgs, OutputFormat};
use crate::config;
use crate::output;

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

    if let Ok(Some(file)) = store.get_file_by_path(&path) {
        match args.format {
            OutputFormat::Json => output::format_file_json(&file),
            OutputFormat::Table => output::format_file_info(&file, args.tracks_only),
        }
        return Ok(());
    }

    // Not in DB — introspect live
    println!("{}", style("File not in database, introspecting...").dim());

    let mut introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    if let Some(fp) = config.ffprobe_path() {
        introspector = introspector.with_ffprobe_path(fp);
    }
    let size = std::fs::metadata(&path)?.len();

    let event = introspector
        .introspect(&path, size, "")
        .map_err(|e| anyhow::anyhow!("Introspection failed: {e}"))?;

    match args.format {
        OutputFormat::Json => output::format_file_json(&event.file),
        OutputFormat::Table => output::format_file_info(&event.file, args.tracks_only),
    }

    Ok(())
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
        };
        let result = run(args);
        assert!(result.is_err());
    }
}
