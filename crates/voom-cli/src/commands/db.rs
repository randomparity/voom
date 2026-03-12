use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::DbCommands;

pub async fn run(cmd: DbCommands) -> Result<()> {
    match cmd {
        DbCommands::Prune => prune().await,
        DbCommands::Vacuum => vacuum().await,
        DbCommands::Reset => reset().await,
    }
}

async fn prune() -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    use voom_domain::storage::StorageTrait;
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

    use voom_domain::storage::StorageTrait;
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

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    if input.trim() != "yes" {
        println!("{}", "Aborted.".dimmed());
        return Ok(());
    }

    std::fs::remove_file(&db_path)?;
    println!("{} Database reset.", "OK".bold().green());

    Ok(())
}
