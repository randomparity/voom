use anyhow::Result;
use comfy_table::{Cell, Table};
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::{DbCommands, OutputFormat};

pub async fn run(cmd: DbCommands) -> Result<()> {
    match cmd {
        DbCommands::Prune => prune().await,
        DbCommands::Vacuum => vacuum().await,
        DbCommands::Reset => reset().await,
        DbCommands::ListBad { path, format } => list_bad(path, format).await,
        DbCommands::PurgeBad => purge_bad().await,
        DbCommands::CleanBad { yes } => clean_bad(yes).await,
    }
}

async fn prune() -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    let count = store
        .prune_missing_files()
        .map_err(|e| anyhow::anyhow!("failed to prune missing files: {e}"))?;

    if count == 0 {
        println!("{}", "No stale entries found.".dimmed());
    } else {
        println!(
            "{} Pruned {} stale entries.",
            "OK".bold().green(),
            count.to_string().bold()
        );
    }

    Ok(())
}

async fn vacuum() -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    store
        .vacuum()
        .map_err(|e| anyhow::anyhow!("failed to vacuum database: {e}"))?;

    println!("{} Database vacuumed.", "OK".bold().green());

    Ok(())
}

async fn reset() -> Result<()> {
    let config = app::load_config()?;
    let db_path = config.data_dir.join("voom.db");

    if !db_path.exists() {
        println!("{}", "No database file found.".dimmed());
        return Ok(());
    }

    // Safety prompt via stderr
    eprintln!(
        "{} This will delete all data in {}",
        "WARNING".bold().red(),
        db_path.display().to_string().bold()
    );
    eprintln!("Type 'yes' to confirm:");

    let input = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await??;

    if input.trim() != "yes" {
        println!("{}", "Aborted.".dimmed());
        return Ok(());
    }

    std::fs::remove_file(&db_path)?;
    // Also remove WAL and SHM companion files to avoid corruption on next open
    let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    println!("{} Database reset.", "OK".bold().green());

    Ok(())
}

async fn list_bad(path: Option<String>, format: OutputFormat) -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    use voom_domain::storage::BadFileFilters;
    let filters = BadFileFilters {
        path_prefix: path,
        ..Default::default()
    };
    let bad_files = store
        .list_bad_files(&filters)
        .map_err(|e| anyhow::anyhow!("failed to list bad files: {e}"))?;

    if bad_files.is_empty() {
        println!("{}", "No bad files recorded.".dimmed());
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
            let mut table = Table::new();
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
            println!("\n{} bad files total.", bad_files.len().to_string().bold());
        }
    }

    Ok(())
}

async fn purge_bad() -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    use voom_domain::storage::BadFileFilters;
    let bad_files = store
        .list_bad_files(&BadFileFilters::default())
        .map_err(|e| anyhow::anyhow!("failed to list bad files: {e}"))?;

    if bad_files.is_empty() {
        println!("{}", "No bad files recorded.".dimmed());
        return Ok(());
    }

    let count = bad_files.len();
    for bf in &bad_files {
        store
            .delete_bad_file(&bf.id)
            .map_err(|e| anyhow::anyhow!("failed to delete bad file entry: {e}"))?;
    }

    println!(
        "{} Purged {} bad file entries from database.",
        "OK".bold().green(),
        count.to_string().bold()
    );

    Ok(())
}

async fn clean_bad(yes: bool) -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    use voom_domain::storage::BadFileFilters;
    let bad_files = store
        .list_bad_files(&BadFileFilters::default())
        .map_err(|e| anyhow::anyhow!("failed to list bad files: {e}"))?;

    if bad_files.is_empty() {
        println!("{}", "No bad files recorded.".dimmed());
        return Ok(());
    }

    let total_size: u64 = bad_files.iter().map(|bf| bf.size).sum();
    let count = bad_files.len();

    println!(
        "Found {} bad files ({}).",
        count.to_string().bold(),
        voom_domain::utils::datetime::format_size(total_size)
    );

    if !yes {
        eprintln!(
            "{} This will delete {} files from disk.",
            "WARNING".bold().red(),
            count
        );
        eprintln!("Type 'yes' to confirm:");

        let input = tokio::task::spawn_blocking(|| {
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf).map(|_| buf)
        })
        .await??;

        if input.trim() != "yes" {
            println!("{}", "Aborted.".dimmed());
            return Ok(());
        }
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
                        "ERROR".red(),
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
                .map_err(|e| anyhow::anyhow!("failed to delete bad file entry: {e}"))?;
        }
    }

    println!(
        "{} {} deleted, {} already missing, {} errors.",
        "Done.".bold().green(),
        deleted.to_string().bold(),
        missing.to_string().dimmed(),
        if errors > 0 {
            errors.to_string().red().to_string()
        } else {
            errors.to_string()
        }
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::app;

    #[test]
    fn db_path_uses_data_dir() {
        let config = app::AppConfig {
            data_dir: std::path::PathBuf::from("/tmp/test-voom"),
            plugins: app::PluginsConfig::default(),
            auth_token: None,
            plugin: std::collections::HashMap::new(),
        };
        let db_path = config.data_dir.join("voom.db");
        assert_eq!(db_path, std::path::PathBuf::from("/tmp/test-voom/voom.db"));
    }

    #[test]
    fn open_store_in_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = app::AppConfig {
            data_dir: dir.path().to_path_buf(),
            plugins: app::PluginsConfig::default(),
            auth_token: None,
            plugin: std::collections::HashMap::new(),
        };
        let store = app::open_store(&config);
        assert!(store.is_ok(), "should open store in temp directory");
    }
}
