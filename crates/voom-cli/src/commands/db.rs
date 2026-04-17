use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;
use voom_domain::storage::BadFileFilters;

use crate::app;
use crate::cli::{DbCommands, OutputFormat};
use crate::config;
use crate::output;

pub async fn run(cmd: DbCommands, global_yes: bool) -> Result<()> {
    match cmd {
        DbCommands::Prune => prune(),
        DbCommands::Vacuum => vacuum(),
        DbCommands::Reset { yes } => reset(yes || global_yes).await,
        DbCommands::ListBad { path, format } => list_bad(path, format),
        DbCommands::PurgeBad => purge_bad(),
        DbCommands::CleanBad { yes } => clean_bad(yes || global_yes).await,
    }
}

fn prune() -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let count = store
        .prune_missing_files()
        .context("failed to prune missing files")?;

    if count == 0 {
        println!("{}", style("No stale entries found.").dim());
    } else {
        println!(
            "{} Marked {} files as missing.",
            style("OK").bold().green(),
            style(count).bold()
        );
    }

    // Purge missing files older than retention period
    let retention_days = config.pruning.retention_days;
    if retention_days > 0 {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days));
        match store.purge_missing(cutoff) {
            Ok(n) if n > 0 => {
                println!(
                    "{} Purged {} missing file records.",
                    style("OK").bold().green(),
                    style(n).bold()
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "purge_missing failed"),
        }
    }

    // Prune health checks using the default retention period
    let retention = i64::from(voom_health_checker::HealthCheckerConfig::default().retention_days);
    let health_pruned = store
        .prune_health_checks(chrono::Utc::now() - chrono::Duration::days(retention))
        .context("failed to prune old health checks")?;

    if health_pruned > 0 {
        println!(
            "{} Pruned {} old health check records.",
            style("OK").bold().green(),
            style(health_pruned).bold()
        );
    }

    Ok(())
}

fn vacuum() -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    store.vacuum().context("failed to vacuum database")?;

    println!("{} Database vacuumed.", style("OK").bold().green());

    Ok(())
}

async fn reset(yes: bool) -> Result<()> {
    let config = config::load_config()?;
    let db_path = config.data_dir.join("voom.db");

    if !db_path.exists() {
        println!("{}", style("No database file found.").dim());
        return Ok(());
    }

    let prompt = format!(
        "{} This will delete all data in {}",
        style("WARNING").bold().red(),
        style(db_path.display()).bold()
    );
    let confirmed = tokio::task::spawn_blocking(move || output::confirm(&prompt, yes)).await??;
    if !confirmed {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    std::fs::remove_file(&db_path)?;
    // Also remove WAL and SHM companion files to avoid corruption on next open
    let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    println!("{} Database reset.", style("OK").bold().green());

    Ok(())
}

fn list_bad(path: Option<String>, format: OutputFormat) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let mut filters = BadFileFilters::default();
    filters.path_prefix = path;
    let bad_files = store
        .list_bad_files(&filters)
        .context("failed to list bad files")?;

    if bad_files.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!("{}", style("No bad files recorded.").dim());
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = bad_files
                .iter()
                .map(|bf| {
                    serde_json::json!({
                        "path": bf.path,
                        "error": bf.error,
                        "error_source": bf.error_source.to_string(),
                        "attempt_count": bf.attempt_count,
                        "size": bf.size,
                        "last_seen_at": bf.last_seen_at.to_rfc3339(),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        OutputFormat::Table => {
            let mut table = output::new_table();
            table.set_header(vec!["Path", "Error", "Source", "Attempts", "Last Seen"]);
            for bf in &bad_files {
                let error_display = if bf.error.len() > 60 {
                    format!("{}...", &bf.error[..57])
                } else {
                    bf.error.clone()
                };
                table.add_row(vec![
                    Cell::new(bf.path.display()),
                    Cell::new(&error_display),
                    Cell::new(bf.error_source.to_string()),
                    Cell::new(bf.attempt_count),
                    Cell::new(bf.last_seen_at.format("%Y-%m-%d %H:%M")),
                ]);
            }
            println!("{table}");
            println!("\n{} bad files total.", style(bad_files.len()).bold());
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            for bf in &bad_files {
                println!("{}", bf.path.display());
            }
        }
    }

    Ok(())
}

fn purge_bad() -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let bad_files = store
        .list_bad_files(&BadFileFilters::default())
        .context("failed to list bad files")?;

    if bad_files.is_empty() {
        println!("{}", style("No bad files recorded.").dim());
        return Ok(());
    }

    let count = bad_files.len();
    for bf in &bad_files {
        store
            .delete_bad_file(&bf.id)
            .context("failed to delete bad file entry")?;
    }

    println!(
        "{} Purged {} bad file entries from database.",
        style("OK").bold().green(),
        style(count).bold()
    );

    Ok(())
}

async fn clean_bad(yes: bool) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let bad_files = store
        .list_bad_files(&BadFileFilters::default())
        .context("failed to list bad files")?;

    if bad_files.is_empty() {
        println!("{}", style("No bad files recorded.").dim());
        return Ok(());
    }

    let total_size: u64 = bad_files.iter().map(|bf| bf.size).sum();
    let count = bad_files.len();

    println!(
        "Found {} bad files ({}).",
        style(count).bold(),
        voom_domain::utils::format::format_size(total_size)
    );

    let prompt = format!(
        "{} This will delete {} files from disk.",
        style("WARNING").bold().red(),
        count
    );
    let confirmed = tokio::task::spawn_blocking(move || output::confirm(&prompt, yes)).await??;
    if !confirmed {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    let mut deleted = 0u64;
    let mut missing = 0u64;
    let mut errors = 0u64;

    for bf in &bad_files {
        let should_delete_entry = if bf.path.exists() {
            match std::fs::remove_file(&bf.path) {
                Ok(()) => {
                    deleted += 1;
                    true
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to delete {}: {e}",
                        style("ERROR").red(),
                        bf.path.display()
                    );
                    errors += 1;
                    false
                }
            }
        } else {
            missing += 1;
            true
        };
        if should_delete_entry {
            store
                .delete_bad_file(&bf.id)
                .context("failed to delete bad file entry")?;
        }
    }

    println!(
        "{} {} deleted, {} already missing, {} errors.",
        style("Done.").bold().green(),
        style(deleted).bold(),
        style(missing).dim(),
        if errors > 0 {
            style(errors).red().to_string()
        } else {
            errors.to_string()
        }
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::app;
    use crate::config;

    #[test]
    fn test_db_path_uses_data_dir() {
        let cfg = config::AppConfig {
            data_dir: std::path::PathBuf::from("/tmp/test-voom"),
            ..Default::default()
        };
        let db_path = cfg.data_dir.join("voom.db");
        assert_eq!(db_path, std::path::PathBuf::from("/tmp/test-voom/voom.db"));
    }

    #[test]
    fn test_open_store_in_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config::AppConfig {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let store = app::open_store(&cfg);
        assert!(store.is_ok(), "should open store in temp directory");
    }
}
