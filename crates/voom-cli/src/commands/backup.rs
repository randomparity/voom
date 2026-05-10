use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use console::style;
use serde::Serialize;
use voom_backup_manager::inventory::{RemoteBackupInventory, RemoteBackupInventoryRecord};
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
        BackupCommands::Restore {
            backup_path,
            from,
            output,
            yes,
        } => restore(
            &backup_path,
            from.as_deref(),
            output.as_deref(),
            yes || global_yes,
        ),
        BackupCommands::Verify {
            destination,
            format,
        } => verify_remote_inventory(&destination, format),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RemoteBackupVerificationStatus {
    Verified,
    Missing,
    SizeMismatch,
    HashMismatch,
    Error,
}

impl RemoteBackupVerificationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Missing => "missing",
            Self::SizeMismatch => "size_mismatch",
            Self::HashMismatch => "hash_mismatch",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
struct RemoteBackupVerificationResult {
    backup_id: uuid::Uuid,
    original_path: PathBuf,
    remote_path: String,
    expected_size: u64,
    actual_size: Option<u64>,
    expected_sha256: Option<String>,
    actual_sha256: Option<String>,
    status: RemoteBackupVerificationStatus,
    message: Option<String>,
}

fn verify_remote_inventory(destination: &str, format: OutputFormat) -> Result<()> {
    let app_config = config::load_config()?;
    let backup_config = backup_config_from_app_config(&app_config)?;
    ensure_backup_destination_configured(&backup_config, destination)?;

    let inventory =
        RemoteBackupInventory::new(RemoteBackupInventory::default_path(&app_config.data_dir));
    let records = inventory
        .list(Some(destination))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let results = verify_remote_records(&backup_config.rclone_path, &records);

    print_remote_verification_results(destination, &results, format)?;
    let problem_count = results
        .iter()
        .filter(|result| result.status != RemoteBackupVerificationStatus::Verified)
        .count();
    if problem_count > 0 {
        bail!("remote backup verification found {problem_count} problem(s)");
    }
    Ok(())
}

fn ensure_backup_destination_configured(
    config: &voom_backup_manager::BackupConfig,
    destination: &str,
) -> Result<()> {
    if config
        .destinations
        .iter()
        .any(|configured| configured.name == destination)
    {
        Ok(())
    } else {
        bail!("backup destination '{destination}' is not configured")
    }
}

fn verify_remote_records(
    rclone_path: &str,
    records: &[RemoteBackupInventoryRecord],
) -> Vec<RemoteBackupVerificationResult> {
    records
        .iter()
        .map(|record| verify_remote_record(rclone_path, record))
        .collect()
}

fn verify_remote_record(
    rclone_path: &str,
    record: &RemoteBackupInventoryRecord,
) -> RemoteBackupVerificationResult {
    match voom_backup_manager::destination::remote_size(rclone_path, &record.remote_path) {
        Ok(actual_size) if actual_size != record.size => {
            remote_verify_result(record, RemoteBackupVerificationStatus::SizeMismatch)
                .with_actual_size(actual_size)
        }
        Ok(actual_size) => verify_remote_hash(rclone_path, record, actual_size),
        Err(e) if is_missing_remote_error(&e.to_string()) => {
            remote_verify_result(record, RemoteBackupVerificationStatus::Missing)
                .with_message(e.to_string())
        }
        Err(e) => remote_verify_result(record, RemoteBackupVerificationStatus::Error)
            .with_message(e.to_string()),
    }
}

fn verify_remote_hash(
    rclone_path: &str,
    record: &RemoteBackupInventoryRecord,
    actual_size: u64,
) -> RemoteBackupVerificationResult {
    let actual_sha256 =
        voom_backup_manager::destination::remote_sha256(rclone_path, &record.remote_path)
            .unwrap_or(None);
    let status = match (&record.sha256, &actual_sha256) {
        (Some(expected), Some(actual)) if expected.to_ascii_lowercase() != *actual => {
            RemoteBackupVerificationStatus::HashMismatch
        }
        _ => RemoteBackupVerificationStatus::Verified,
    };
    remote_verify_result(record, status)
        .with_actual_size(actual_size)
        .with_actual_sha256(actual_sha256)
}

fn is_missing_remote_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "not found",
        "no such file",
        "does not exist",
        "doesn't exist",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn remote_verify_result(
    record: &RemoteBackupInventoryRecord,
    status: RemoteBackupVerificationStatus,
) -> RemoteBackupVerificationResult {
    RemoteBackupVerificationResult {
        backup_id: record.backup_id,
        original_path: record.original_path.clone(),
        remote_path: record.remote_path.clone(),
        expected_size: record.size,
        actual_size: None,
        expected_sha256: record.sha256.clone(),
        actual_sha256: None,
        status,
        message: None,
    }
}

impl RemoteBackupVerificationResult {
    fn with_actual_size(mut self, actual_size: u64) -> Self {
        self.actual_size = Some(actual_size);
        self
    }

    fn with_actual_sha256(mut self, actual_sha256: Option<String>) -> Self {
        self.actual_sha256 = actual_sha256;
        self
    }

    fn with_message(mut self, message: String) -> Self {
        self.message = Some(message);
        self
    }
}

fn print_remote_verification_results(
    destination: &str,
    results: &[RemoteBackupVerificationResult],
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => print_remote_verification_json(results),
        OutputFormat::Table => print_remote_verification_table(destination, results),
        OutputFormat::Plain | OutputFormat::Csv => {
            for result in results {
                println!("{}\t{}", result.status.as_str(), result.remote_path);
            }
            Ok(())
        }
    }
}

fn print_remote_verification_json(results: &[RemoteBackupVerificationResult]) -> Result<()> {
    let json: Vec<serde_json::Value> = results
        .iter()
        .map(|result| {
            serde_json::json!({
                "backup_id": result.backup_id,
                "original_path": result.original_path.display().to_string(),
                "remote_path": result.remote_path,
                "expected_size": result.expected_size,
                "actual_size": result.actual_size,
                "expected_sha256": result.expected_sha256,
                "actual_sha256": result.actual_sha256,
                "status": result.status,
                "message": result.message,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

fn print_remote_verification_table(
    destination: &str,
    results: &[RemoteBackupVerificationResult],
) -> Result<()> {
    println!(
        "{} remote backup(s) verified for {}:\n",
        style(results.len()).bold(),
        style(destination).bold()
    );
    let mut table = output::new_table();
    table.set_header(vec!["Status", "Expected", "Actual", "Hash", "Remote Path"]);
    for result in results {
        table.add_row(vec![
            comfy_table::Cell::new(result.status.as_str()),
            comfy_table::Cell::new(format::format_size(result.expected_size)),
            comfy_table::Cell::new(
                result
                    .actual_size
                    .map_or_else(|| "-".to_string(), format::format_size),
            ),
            comfy_table::Cell::new(hash_status(result)),
            comfy_table::Cell::new(&result.remote_path),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn hash_status(result: &RemoteBackupVerificationResult) -> &'static str {
    match (&result.expected_sha256, &result.actual_sha256) {
        (Some(_), Some(_)) => "checked",
        (Some(_), None) => "unavailable",
        (None, _) => "not_recorded",
    }
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

fn restore(backup_path: &Path, from: Option<&str>, output: Option<&Path>, yes: bool) -> Result<()> {
    if let Some(destination) = from {
        return restore_remote(backup_path, destination, output, yes);
    }
    if output.is_some() {
        anyhow::bail!("--output requires --from for remote restore");
    }

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

fn restore_remote(
    original_path: &Path,
    destination: &str,
    output: Option<&Path>,
    yes: bool,
) -> Result<()> {
    let app_config = config::load_config()?;
    let backup_config = backup_config_from_app_config(&app_config)?;
    if !backup_config
        .destinations
        .iter()
        .any(|configured| configured.name == destination)
    {
        anyhow::bail!("backup destination '{destination}' is not configured");
    }

    let inventory =
        RemoteBackupInventory::new(RemoteBackupInventory::default_path(&app_config.data_dir));
    let records = inventory
        .list(Some(destination))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let record = select_remote_restore_record(&records, original_path, destination)?;
    let output_path = output.unwrap_or(original_path);

    let prompt = format!(
        "Restore remote backup {} to {}?",
        style(&record.remote_path).cyan(),
        style(output_path.display()).cyan()
    );
    if !output::confirm(&prompt, yes)? {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    let temp_path = temporary_restore_path(output_path)?;
    voom_backup_manager::destination::download_with_rclone(
        &backup_config.rclone_path,
        &record.remote_path,
        &temp_path,
        record.size,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::rename(&temp_path, output_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to move restored backup {} to {}: {e}",
            temp_path.display(),
            output_path.display()
        )
    })?;

    println!(
        "{} Restored remote backup to {}",
        style("OK").bold().green(),
        style(output_path.display()).cyan()
    );

    Ok(())
}

pub(crate) fn backup_config_from_app_config(
    config: &config::AppConfig,
) -> Result<voom_backup_manager::BackupConfig> {
    let value = config.plugin.get("backup-manager").map_or_else(
        || serde_json::json!({}),
        |table| serde_json::to_value(table).unwrap_or_else(|_| serde_json::json!({})),
    );
    let backup_config: voom_backup_manager::BackupConfig = serde_json::from_value(value)?;
    backup_config
        .validate()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(backup_config)
}

fn temporary_restore_path(output_path: &Path) -> Result<PathBuf> {
    let parent = output_path.parent().unwrap_or(Path::new("."));
    let file_name = output_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("restore output path has no file name"))?
        .to_string_lossy();
    Ok(parent.join(format!(".{file_name}.voom-restore.tmp")))
}

fn select_remote_restore_record<'a>(
    records: &'a [RemoteBackupInventoryRecord],
    original_path: &Path,
    destination: &str,
) -> Result<&'a RemoteBackupInventoryRecord> {
    let matches: Vec<&RemoteBackupInventoryRecord> = records
        .iter()
        .filter(|record| {
            record.destination_name == destination && record.original_path == original_path
        })
        .collect();
    match matches.as_slice() {
        [record] => Ok(*record),
        [] => anyhow::bail!(
            "no remote backup found for {} on destination '{}'",
            original_path.display(),
            destination
        ),
        _ => anyhow::bail!(
            "multiple remote backups found for {} on destination '{}'; restore is ambiguous",
            original_path.display(),
            destination
        ),
    }
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
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;
    use voom_backup_manager::destination::BackupDestinationConfig;
    use voom_backup_manager::inventory::RemoteBackupInventoryStatus;

    fn remote_record(original_path: &Path, destination: &str) -> RemoteBackupInventoryRecord {
        remote_record_with_path(original_path, destination, "movie.vbak", 5, None)
    }

    fn remote_record_with_path(
        original_path: &Path,
        destination: &str,
        path: &str,
        size: u64,
        sha256: Option<String>,
    ) -> RemoteBackupInventoryRecord {
        RemoteBackupInventoryRecord {
            backup_id: uuid::Uuid::new_v4(),
            original_path: original_path.to_path_buf(),
            local_backup_path: PathBuf::from("/backups/movie.vbak"),
            destination_name: destination.to_string(),
            remote_path: format!("{destination}:voom/{path}"),
            size,
            sha256,
            uploaded_at: chrono::Utc::now(),
            verified_at: Some(chrono::Utc::now()),
            status: RemoteBackupInventoryStatus::Verified,
        }
    }

    fn make_executable(path: &Path) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn fake_rclone(dir: &Path) -> PathBuf {
        let rclone = dir.join("fake-rclone");
        let remote_root = dir.join("remote");
        std::fs::create_dir_all(&remote_root).unwrap();
        std::fs::write(
            &rclone,
            format!(
                "#!/usr/bin/env bash\n\
                 set -euo pipefail\n\
                 root=\"{}\"\n\
                 cmd=\"$1\"\n\
                 shift\n\
                 case \"$cmd\" in\n\
                   size)\n\
                     shift\n\
                     file=\"$root/$1\"\n\
                     if [[ ! -f \"$file\" ]]; then\n\
                       echo \"not found: $1\" >&2\n\
                       exit 1\n\
                     fi\n\
                     bytes=\"$(wc -c < \"$file\" | tr -d ' ')\"\n\
                     printf '{{\"bytes\":%s,\"count\":1}}\\n' \"$bytes\"\n\
                     ;;\n\
                   hashsum)\n\
                     shift\n\
                     case \"$1\" in\n\
                       *hash-mismatch*) printf '{}  %s\\n' \"$1\" ;;\n\
                       *) printf '{}  %s\\n' \"$1\" ;;\n\
                     esac\n\
                     ;;\n\
                 esac\n",
                remote_root.display(),
                "b".repeat(64),
                "a".repeat(64),
            ),
        )
        .unwrap();
        make_executable(&rclone);
        rclone
    }

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

    #[test]
    fn select_remote_restore_record_finds_unique_match() {
        let original = Path::new("/media/movie.mkv");
        let records = vec![
            remote_record(original, "offsite"),
            remote_record(Path::new("/media/other.mkv"), "offsite"),
        ];

        let selected = select_remote_restore_record(&records, original, "offsite").unwrap();

        assert_eq!(selected.original_path, original);
        assert_eq!(selected.destination_name, "offsite");
    }

    #[test]
    fn select_remote_restore_record_rejects_missing_match() {
        let original = Path::new("/media/movie.mkv");
        let records = vec![remote_record(original, "offsite")];

        let err = select_remote_restore_record(&records, original, "archive").unwrap_err();

        assert!(err.to_string().contains("no remote backup found"));
    }

    #[test]
    fn select_remote_restore_record_rejects_ambiguous_match() {
        let original = Path::new("/media/movie.mkv");
        let records = vec![
            remote_record(original, "offsite"),
            remote_record(original, "offsite"),
        ];

        let err = select_remote_restore_record(&records, original, "offsite").unwrap_err();

        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn verify_remote_records_reports_verified_backup() {
        let dir = TempDir::new().unwrap();
        let rclone = fake_rclone(dir.path());
        let remote_file = dir.path().join("remote/offsite:voom/movie.vbak");
        std::fs::create_dir_all(remote_file.parent().unwrap()).unwrap();
        std::fs::write(remote_file, b"movie").unwrap();
        let records = vec![remote_record_with_path(
            Path::new("/media/movie.mkv"),
            "offsite",
            "movie.vbak",
            5,
            Some("a".repeat(64)),
        )];

        let results = verify_remote_records(&rclone.display().to_string(), &records);

        assert_eq!(results[0].status, RemoteBackupVerificationStatus::Verified);
        assert_eq!(results[0].actual_size, Some(5));
        assert_eq!(results[0].actual_sha256, Some("a".repeat(64)));
    }

    #[test]
    fn verify_remote_records_reports_missing_backup() {
        let dir = TempDir::new().unwrap();
        let rclone = fake_rclone(dir.path());
        let records = vec![remote_record(Path::new("/media/movie.mkv"), "offsite")];

        let results = verify_remote_records(&rclone.display().to_string(), &records);

        assert_eq!(results[0].status, RemoteBackupVerificationStatus::Missing);
        assert!(results[0].message.as_deref().unwrap().contains("not found"));
    }

    #[test]
    fn verify_remote_records_reports_size_mismatch() {
        let dir = TempDir::new().unwrap();
        let rclone = fake_rclone(dir.path());
        let remote_file = dir.path().join("remote/offsite:voom/size-mismatch.vbak");
        std::fs::create_dir_all(remote_file.parent().unwrap()).unwrap();
        std::fs::write(remote_file, b"too-large").unwrap();
        let records = vec![remote_record_with_path(
            Path::new("/media/movie.mkv"),
            "offsite",
            "size-mismatch.vbak",
            5,
            None,
        )];

        let results = verify_remote_records(&rclone.display().to_string(), &records);

        assert_eq!(
            results[0].status,
            RemoteBackupVerificationStatus::SizeMismatch
        );
        assert_eq!(results[0].actual_size, Some(9));
    }

    #[test]
    fn verify_remote_records_reports_hash_mismatch() {
        let dir = TempDir::new().unwrap();
        let rclone = fake_rclone(dir.path());
        let remote_file = dir.path().join("remote/offsite:voom/hash-mismatch.vbak");
        std::fs::create_dir_all(remote_file.parent().unwrap()).unwrap();
        std::fs::write(remote_file, b"movie").unwrap();
        let records = vec![remote_record_with_path(
            Path::new("/media/movie.mkv"),
            "offsite",
            "hash-mismatch.vbak",
            5,
            Some("a".repeat(64)),
        )];

        let results = verify_remote_records(&rclone.display().to_string(), &records);

        assert_eq!(
            results[0].status,
            RemoteBackupVerificationStatus::HashMismatch
        );
        assert_eq!(results[0].actual_sha256, Some("b".repeat(64)));
    }

    #[test]
    fn ensure_backup_destination_configured_rejects_missing_destination() {
        let mut config = voom_backup_manager::BackupConfig::default();
        config.destinations = vec![BackupDestinationConfig::rclone("offsite", "b2:voom")];

        let err = ensure_backup_destination_configured(&config, "archive").unwrap_err();

        assert!(err.to_string().contains("not configured"));
    }
}
