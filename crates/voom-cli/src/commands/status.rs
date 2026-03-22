use anyhow::Result;
use console::style;

use crate::app;
use crate::config;
use crate::stats;

pub fn run() -> Result<()> {
    let config = config::load_config()?;

    println!("{}", style("VOOM Status").bold().underlined());
    println!();

    // Database stats
    match app::bootstrap_kernel_with_store(&config) {
        Ok((kernel, store)) => {
            let files = store
                .list_files(&voom_domain::FileFilters::default())
                .map_err(|e| anyhow::anyhow!("failed to list files from database: {e}"))?;

            let total_size: u64 = files.iter().map(|f| f.size).sum();

            println!("{}", style("Library:").bold());
            println!(
                "  {} files, {}",
                style(files.len()).cyan(),
                style(voom_domain::utils::datetime::format_size(total_size)).cyan()
            );

            // Bad file count
            match store.count_bad_files() {
                Ok(0) => {}
                Ok(n) => {
                    println!(
                        "  {} bad files (run {} to see details)",
                        style(n).red(),
                        style("voom db list-bad").bold()
                    );
                }
                Err(e) => {
                    tracing::warn!("failed to count bad files: {e}");
                }
            }

            // Container breakdown (top 5)
            let sorted = stats::container_counts(&files);
            if !sorted.is_empty() {
                let summary: Vec<String> = sorted
                    .iter()
                    .take(5)
                    .map(|(c, n)| format!("{c}: {n}"))
                    .collect();
                println!("  Containers: {}", summary.join(", "));
            }

            // Plugin count
            let plugin_count = kernel.registry.plugin_names().len();
            println!();
            println!("{}", style("Plugins:").bold());
            println!("  {plugin_count} registered");
        }
        Err(e) => {
            println!("{} Cannot initialize kernel: {e}", style("ERROR").red());
        }
    }

    // Config path
    println!();
    println!("{}", style("Paths:").bold());
    println!("  Config: {}", config::config_path().display());
    println!("  Data:   {}", config.data_dir.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config;
    use voom_domain::media::{Container, MediaFile};

    #[test]
    fn test_container_counts_from_output_module() {
        let files = vec![
            MediaFile::new(std::path::PathBuf::from("/a.mkv")).with_container(Container::Mkv),
            MediaFile::new(std::path::PathBuf::from("/b.mkv")).with_container(Container::Mkv),
            MediaFile::new(std::path::PathBuf::from("/c.mp4")).with_container(Container::Mp4),
        ];
        let counts = crate::stats::container_counts(&files);
        assert_eq!(counts[0], ("mkv".to_string(), 2));
        assert_eq!(counts[1], ("mp4".to_string(), 1));
    }

    #[test]
    fn test_status_paths_display() {
        let cfg = config::AppConfig::default();
        let config_path = config::config_path();
        // Both paths should be displayable without panic
        let _ = format!("Config: {}", config_path.display());
        let _ = format!("Data: {}", cfg.data_dir.display());
    }
}
