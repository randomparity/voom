use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;

pub async fn run() -> Result<()> {
    let config = app::load_config()?;

    println!("{}", "VOOM Status".bold().underline());
    println!();

    // Database stats
    match app::bootstrap_kernel(&config) {
        Ok(kernel) => {
            match app::open_store(&config) {
                Ok(store) => {
                    use voom_domain::storage::StorageTrait;
                    let files = store
                        .list_files(&voom_domain::FileFilters::default())
                        .map_err(|e| anyhow::anyhow!("{e}"))?;

                    let total_size: u64 = files.iter().map(|f| f.size).sum();

                    println!("{}", "Library:".bold());
                    println!(
                        "  {} files, {}",
                        files.len().to_string().cyan(),
                        voom_domain::utils::datetime::format_size(total_size).cyan()
                    );

                    // Container breakdown (top 5)
                    let mut containers = std::collections::HashMap::new();
                    for file in &files {
                        *containers
                            .entry(file.container.as_str().to_string())
                            .or_insert(0u32) += 1;
                    }
                    let mut sorted: Vec<_> = containers.into_iter().collect();
                    sorted.sort_by(|a, b| b.1.cmp(&a.1));
                    if !sorted.is_empty() {
                        let summary: Vec<String> = sorted
                            .iter()
                            .take(5)
                            .map(|(c, n)| format!("{c}: {n}"))
                            .collect();
                        println!("  Containers: {}", summary.join(", "));
                    }
                }
                Err(e) => {
                    println!("{} Cannot access database: {e}", "WARNING".yellow());
                }
            }

            // Plugin count
            let plugin_count = kernel.registry.plugin_names().len();
            println!();
            println!("{}", "Plugins:".bold());
            println!("  {plugin_count} registered");
        }
        Err(e) => {
            println!("{} Cannot initialize kernel: {e}", "ERROR".red());
        }
    }

    // Config path
    println!();
    println!("{}", "Paths:".bold());
    println!("  Config: {}", app::config_path().display());
    println!("  Data:   {}", config.data_dir.display());

    Ok(())
}
