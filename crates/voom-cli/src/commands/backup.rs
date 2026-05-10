use std::path::{Path, PathBuf};

use anyhow::Result;
use console::style;
use voom_domain::utils::format;

use crate::cli::{BackupCommands, OutputFormat};
use crate::config;
use crate::output;

/// A discovered `.vbak` file on disk.
struct VbakEntry {
    backup_path: PathBuf,
    original_name: String,
    size: u64,
}

pub fn run(cmd: BackupCommands, global_yes: bool) -> Result<()> {
    match cmd {
        BackupCommands::List {
            paths,
            destination,
            format,
        } => list(&paths, destination.as_deref(), format),
        BackupCommands::Restore { backup_path, yes } => restore(&backup_path, yes || global_yes),
        BackupCommands::Cleanup { paths, yes } => cleanup(&paths, yes || global_yes),
    }
}

fn list(roots: &[PathBuf], destination: Option<&str>, format: OutputFormat) -> Result<()> {
    if let Some(destination) = destination {
        return list_remote_inventory(destination, format);
    }
    if roots.is_empty() {
        anyhow::bail!("backup list requires at least one path unless --destination is provided");
    }

    let mut all_entries = Vec::new();
    for root in roots {
        let entries = scan_vbak_files(root)?;
        all_entries.extend(entries);
    }

    if all_entries.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style("No .vbak files found under the given path(s).").dim()
        );
        return Ok(());
    }

    let entries = all_entries;

    match format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "backup_path": e.backup_path.display().to_string(),
                        "original_name": e.original_name,
                        "size": e.size,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json)
                    .expect("serde_json::Value serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            println!("{} backup(s) found:\n", style(entries.len()).bold());

            let mut table = output::new_table();
            table.set_header(vec!["Original", "Size", "Backup Path"]);
            for entry in &entries {
                table.add_row(vec![
                    comfy_table::Cell::new(&entry.original_name),
                    comfy_table::Cell::new(format::format_size(entry.size)),
                    comfy_table::Cell::new(entry.backup_path.display()),
                ]);
            }
            println!("{table}");
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            for entry in &entries {
                println!("{}", entry.backup_path.display());
            }
        }
    }

    Ok(())
}

fn list_remote_inventory(destination: &str, format: OutputFormat) -> Result<()> {
    let config = config::load_config()?;
    let inventory = voom_backup_manager::inventory::RemoteBackupInventory::new(
        voom_backup_manager::inventory::RemoteBackupInventory::default_path(&config.data_dir),
    );
    let entries = inventory
        .list(Some(destination))
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if entries.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style(format!(
                "No remote backups found for destination '{destination}'."
            ))
            .dim()
        );
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "backup_id": e.backup_id,
                        "original_path": e.original_path.display().to_string(),
                        "local_backup_path": e.local_backup_path.display().to_string(),
                        "destination_name": e.destination_name,
                        "remote_path": e.remote_path,
                        "size": e.size,
                        "uploaded_at": e.uploaded_at,
                        "verified_at": e.verified_at,
                        "status": e.status.as_str(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json)
                    .expect("serde_json::Value serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            println!(
                "{} remote backup(s) found for {}:\n",
                style(entries.len()).bold(),
                style(destination).bold()
            );

            let mut table = output::new_table();
            table.set_header(vec!["Original", "Size", "Status", "Remote Path"]);
            for entry in &entries {
                table.add_row(vec![
                    comfy_table::Cell::new(entry.original_path.display()),
                    comfy_table::Cell::new(format::format_size(entry.size)),
                    comfy_table::Cell::new(entry.status.as_str()),
                    comfy_table::Cell::new(&entry.remote_path),
                ]);
            }
            println!("{table}");
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            for entry in &entries {
                println!("{}", entry.remote_path);
            }
        }
    }

    Ok(())
}

fn restore(backup_path: &Path, yes: bool) -> Result<()> {
    let original_name = derive_original_name(backup_path).ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot derive original filename from: {}. \
             Expected format: <name>.<timestamp>.vbak",
            backup_path.display()
        )
    })?;

    // The original file goes in the parent of the .voom-backup dir,
    // or the same directory as the backup file if not in .voom-backup.
    let original_path = derive_original_path(backup_path, &original_name);

    let prompt = format!(
        "Restore {} to {}?",
        style(backup_path.display()).cyan(),
        style(original_path.display()).cyan()
    );
    if !output::confirm(&prompt, yes)? {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    voom_backup_manager::backup::restore_from_paths(backup_path, &original_path)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "{} Restored to {}",
        style("OK").bold().green(),
        style(original_path.display()).cyan()
    );

    Ok(())
}

fn cleanup(roots: &[PathBuf], yes: bool) -> Result<()> {
    let mut all_entries = Vec::new();
    for root in roots {
        let entries = scan_vbak_files(root)?;
        all_entries.extend(entries);
    }

    if all_entries.is_empty() {
        println!(
            "{}",
            style("No .vbak files found under the given path(s).").dim()
        );
        return Ok(());
    }

    let entries = all_entries;

    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    eprintln!(
        "Found {} backup(s) totaling {}",
        style(entries.len()).bold(),
        style(format::format_size(total_size)).bold()
    );

    if !output::confirm("Confirm deletion?", yes)? {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    let mut removed = 0u64;
    let mut errors = 0u64;
    for entry in &entries {
        match voom_backup_manager::backup::remove_vbak_file(&entry.backup_path) {
            Ok(()) => removed += 1,
            Err(e) => {
                eprintln!(
                    "{} {}: {e}",
                    style("ERROR").red(),
                    entry.backup_path.display()
                );
                errors += 1;
            }
        }
    }

    println!(
        "{} Removed {removed} backup(s){}",
        style("OK").bold().green(),
        if errors > 0 {
            format!(", {errors} error(s)")
        } else {
            String::new()
        }
    );

    Ok(())
}

/// Scan for `.vbak` files under a directory.
///
/// Looks for sibling `.voom-backup/` directories containing `*.vbak` files.
fn scan_vbak_files(root: &Path) -> Result<Vec<VbakEntry>> {
    let mut entries = Vec::new();
    scan_dir_recursive(root, &mut entries)?;

    entries.sort_by(|a, b| a.backup_path.cmp(&b.backup_path));
    Ok(entries)
}

fn scan_dir_recursive(dir: &Path, entries: &mut Vec<VbakEntry>) -> Result<()> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Ok(());
    };

    for entry in read_dir {
        let Ok(entry) = entry else {
            continue;
        };

        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let path = entry.path();
        if name == ".voom-backup" {
            collect_vbak_in_dir(&path, entries);
        } else {
            let name_str = name.to_string_lossy();
            if !name_str.starts_with('.') {
                scan_dir_recursive(&path, entries)?;
            }
        }
    }

    Ok(())
}

fn collect_vbak_in_dir(dir: &Path, entries: &mut Vec<VbakEntry>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in read_dir {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("vbak") {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let original_name = derive_original_name(&path).unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });
            entries.push(VbakEntry {
                backup_path: path,
                original_name,
                size,
            });
        }
    }
}

/// Derive the original filename from a backup path by stripping the
/// `.YYYYMMDDHHMMSS.vbak` suffix.
///
/// Example: `movie.mkv.20260329120000.vbak` -> `movie.mkv`
fn derive_original_name(backup_path: &Path) -> Option<String> {
    let filename = backup_path.file_name()?.to_string_lossy();

    // Strip `.vbak` suffix
    let without_vbak = filename.strip_suffix(".vbak")?;

    // Strip `.YYYYMMDDHHMMSS` (14 digits preceded by a dot)
    let dot_pos = without_vbak.rfind('.')?;
    let timestamp = &without_vbak[dot_pos + 1..];
    if timestamp.len() == 14 && timestamp.chars().all(|c| c.is_ascii_digit()) {
        Some(without_vbak[..dot_pos].to_string())
    } else {
        None
    }
}

/// Derive the full original path for a backup file.
///
/// If the backup is inside a `.voom-backup/` directory, the original goes
/// in the parent of that directory. Otherwise it goes next to the backup.
fn derive_original_path(backup_path: &Path, original_name: &str) -> PathBuf {
    let parent = backup_path.parent().unwrap_or(Path::new("."));
    if parent.file_name().and_then(|n| n.to_str()) == Some(".voom-backup") {
        parent
            .parent()
            .unwrap_or(Path::new("."))
            .join(original_name)
    } else {
        parent.join(original_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_derive_original_name_valid() {
        let path = Path::new("/media/.voom-backup/movie.mkv.20260329120000.vbak");
        assert_eq!(derive_original_name(path), Some("movie.mkv".to_string()));
    }

    #[test]
    fn test_derive_original_name_complex() {
        let path = Path::new("/media/.voom-backup/My Movie (2024).mkv.20260101235959.vbak");
        assert_eq!(
            derive_original_name(path),
            Some("My Movie (2024).mkv".to_string())
        );
    }

    #[test]
    fn test_derive_original_name_no_timestamp() {
        let path = Path::new("/media/.voom-backup/movie.mkv.vbak");
        assert_eq!(derive_original_name(path), None);
    }

    #[test]
    fn test_derive_original_name_bad_timestamp_length() {
        let path = Path::new("/media/.voom-backup/movie.mkv.12345.vbak");
        assert_eq!(derive_original_name(path), None);
    }

    #[test]
    fn test_derive_original_path_in_voom_backup() {
        let backup = Path::new("/media/movies/.voom-backup/movie.mkv.20260329120000.vbak");
        let result = derive_original_path(backup, "movie.mkv");
        assert_eq!(result, PathBuf::from("/media/movies/movie.mkv"));
    }

    #[test]
    fn test_derive_original_path_not_in_voom_backup() {
        let backup = Path::new("/tmp/backups/movie.mkv.20260329120000.vbak");
        let result = derive_original_path(backup, "movie.mkv");
        assert_eq!(result, PathBuf::from("/tmp/backups/movie.mkv"));
    }

    #[test]
    fn test_scan_finds_vbak_files() {
        let dir = TempDir::new().unwrap();
        let backup_dir = dir.path().join("movies").join(".voom-backup");
        fs::create_dir_all(&backup_dir).unwrap();
        fs::write(
            backup_dir.join("test.mkv.20260329120000.vbak"),
            b"backup data",
        )
        .unwrap();

        let entries = scan_vbak_files(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].original_name, "test.mkv");
        assert_eq!(entries[0].size, 11);
    }

    #[test]
    fn test_scan_empty_directory() {
        let dir = TempDir::new().unwrap();
        let entries = scan_vbak_files(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_scan_nested_vbak_files() {
        let dir = TempDir::new().unwrap();

        let backup1 = dir.path().join("movies").join(".voom-backup");
        fs::create_dir_all(&backup1).unwrap();
        fs::write(backup1.join("a.mkv.20260101000000.vbak"), b"data").unwrap();

        let backup2 = dir.path().join("tv").join("show").join(".voom-backup");
        fs::create_dir_all(&backup2).unwrap();
        fs::write(backup2.join("b.mkv.20260201000000.vbak"), b"data2").unwrap();

        let entries = scan_vbak_files(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);
    }
}
