use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;
use crate::output;

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
                        .map_err(|e| anyhow::anyhow!("failed to list files from database: {e}"))?;

                    let total_size: u64 = files.iter().map(|f| f.size).sum();

                    println!("{}", "Library:".bold());
                    println!(
                        "  {} files, {}",
                        files.len().to_string().cyan(),
                        voom_domain::utils::datetime::format_size(total_size).cyan()
                    );

                    // Bad file count
                    match store.count_bad_files() {
                        Ok(0) => {}
                        Ok(n) => {
                            println!(
                                "  {} bad files (run {} to see details)",
                                n.to_string().red(),
                                "voom db list-bad".bold()
                            );
                        }
                        Err(e) => {
                            tracing::warn!("failed to count bad files: {e}");
                        }
                    }

                    // Container breakdown (top 5)
                    let sorted = output::container_counts(&files);
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

#[cfg(test)]
mod tests {
    use crate::app;
    use voom_domain::media::{Container, MediaFile};

    #[test]
    fn container_counts_from_output_module() {
        let files = vec![
            MediaFile::new(std::path::PathBuf::from("/a.mkv")).with_container(Container::Mkv),
            MediaFile::new(std::path::PathBuf::from("/b.mkv")).with_container(Container::Mkv),
            MediaFile::new(std::path::PathBuf::from("/c.mp4")).with_container(Container::Mp4),
        ];
        let counts = crate::output::container_counts(&files);
        assert_eq!(counts[0], ("mkv".to_string(), 2));
        assert_eq!(counts[1], ("mp4".to_string(), 1));
    }

    #[test]
    fn status_paths_display() {
        let config = app::AppConfig::default();
        let config_path = app::config_path();
        // Both paths should be displayable without panic
        let _ = format!("Config: {}", config_path.display());
        let _ = format!("Data: {}", config.data_dir.display());
    }
}
