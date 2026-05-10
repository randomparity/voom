//! Persistent remote backup inventory.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use voom_domain::errors::Result;

use crate::plugin_err;

/// Persisted status for one remote backup object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteBackupInventoryStatus {
    Uploaded,
    Verified,
}

impl RemoteBackupInventoryStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::Verified => "verified",
        }
    }
}

/// One persisted remote backup object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBackupInventoryRecord {
    pub backup_id: Uuid,
    pub original_path: PathBuf,
    pub local_backup_path: PathBuf,
    pub destination_name: String,
    pub remote_path: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    pub uploaded_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub status: RemoteBackupInventoryStatus,
}

/// JSONL inventory file for remote backup records.
#[derive(Debug, Clone)]
pub struct RemoteBackupInventory {
    path: PathBuf,
}

impl RemoteBackupInventory {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn default_path(data_dir: &Path) -> PathBuf {
        data_dir.join("backup-manager").join("remote-backups.jsonl")
    }

    pub fn append(&self, record: &RemoteBackupInventoryRecord) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                plugin_err(format!(
                    "failed to create remote backup inventory directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| {
                plugin_err(format!(
                    "failed to open remote backup inventory {}: {e}",
                    self.path.display()
                ))
            })?;
        let json = serde_json::to_string(record)
            .expect("RemoteBackupInventoryRecord serialization cannot fail");
        writeln!(file, "{json}").map_err(|e| {
            plugin_err(format!(
                "failed to write remote backup inventory {}: {e}",
                self.path.display()
            ))
        })?;
        Ok(())
    }

    pub fn list(&self, destination: Option<&str>) -> Result<Vec<RemoteBackupInventoryRecord>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(plugin_err(format!(
                    "failed to open remote backup inventory {}: {e}",
                    self.path.display()
                )));
            }
        };

        let mut records = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| {
                plugin_err(format!(
                    "failed to read remote backup inventory {}: {e}",
                    self.path.display()
                ))
            })?;
            let record: RemoteBackupInventoryRecord = serde_json::from_str(&line).map_err(|e| {
                plugin_err(format!(
                    "failed to parse remote backup inventory {}: {e}",
                    self.path.display()
                ))
            })?;
            if destination.is_none_or(|name| record.destination_name == name) {
                records.push(record);
            }
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;

    use super::*;

    fn record(destination_name: &str, remote_path: &str) -> RemoteBackupInventoryRecord {
        RemoteBackupInventoryRecord {
            backup_id: uuid::Uuid::new_v4(),
            original_path: PathBuf::from("/media/movie.mkv"),
            local_backup_path: PathBuf::from("/backups/movie.mkv.vbak"),
            destination_name: destination_name.to_string(),
            remote_path: remote_path.to_string(),
            size: 42,
            sha256: Some("a".repeat(64)),
            uploaded_at: Utc::now(),
            verified_at: Some(Utc::now()),
            status: RemoteBackupInventoryStatus::Verified,
        }
    }

    #[test]
    fn append_and_list_inventory_records() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = RemoteBackupInventory::new(dir.path().join("remote-backups.jsonl"));

        inventory
            .append(&record("offsite", "b2:voom/a.vbak"))
            .unwrap();
        inventory
            .append(&record("archive", "s3:voom/b.vbak"))
            .unwrap();

        let records = inventory.list(None).unwrap();

        assert_eq!(records.len(), 2);
    }

    #[test]
    fn list_filters_by_destination() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = RemoteBackupInventory::new(dir.path().join("remote-backups.jsonl"));

        inventory
            .append(&record("offsite", "b2:voom/a.vbak"))
            .unwrap();
        inventory
            .append(&record("archive", "s3:voom/b.vbak"))
            .unwrap();

        let records = inventory.list(Some("archive")).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].destination_name, "archive");
    }

    #[test]
    fn list_missing_destination_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = RemoteBackupInventory::new(dir.path().join("remote-backups.jsonl"));

        inventory
            .append(&record("offsite", "b2:voom/a.vbak"))
            .unwrap();

        let records = inventory.list(Some("missing")).unwrap();

        assert!(records.is_empty());
    }

    #[test]
    fn missing_inventory_lists_empty() {
        let dir = tempfile::tempdir().unwrap();
        let inventory = RemoteBackupInventory::new(dir.path().join("missing.jsonl"));

        let records = inventory.list(Some("offsite")).unwrap();

        assert!(records.is_empty());
    }
}
