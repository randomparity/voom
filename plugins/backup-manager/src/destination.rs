//! Remote backup destination configuration and command helpers.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use voom_domain::errors::Result;

use crate::plugin_err;

/// Backup destination kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationKind {
    Local,
    Rclone,
    S3,
    Sftp,
    Webdav,
}

impl DestinationKind {
    #[must_use]
    pub fn is_rclone_backed(self) -> bool {
        match self {
            Self::Local => false,
            Self::Rclone | Self::S3 | Self::Sftp | Self::Webdav => true,
        }
    }
}

/// One configured backup destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDestinationConfig {
    pub name: String,
    #[serde(default = "default_destination_kind")]
    pub kind: DestinationKind,
    pub remote: Option<String>,
    pub bandwidth_limit: Option<String>,
}

fn default_destination_kind() -> DestinationKind {
    DestinationKind::Local
}

impl BackupDestinationConfig {
    #[must_use]
    pub fn rclone(name: impl Into<String>, remote: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: DestinationKind::Rclone,
            remote: Some(remote.into()),
            bandwidth_limit: None,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(plugin_err("backup destination name cannot be empty"));
        }
        if self.kind.is_rclone_backed()
            && self
                .remote
                .as_ref()
                .is_none_or(|remote| remote.trim().is_empty())
        {
            return Err(plugin_err(format!(
                "backup destination '{}' requires remote",
                self.name
            )));
        }
        Ok(())
    }
}

/// Shared backup destination options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDestinationsConfig {
    #[serde(default = "default_verify_after_upload")]
    pub verify_after_upload: bool,
    #[serde(default = "default_block_on_remote_failure")]
    pub block_on_remote_failure: bool,
    #[serde(default = "default_rclone_path")]
    pub rclone_path: String,
    #[serde(default)]
    pub destinations: Vec<BackupDestinationConfig>,
}

impl BackupDestinationsConfig {
    pub fn validate(&self) -> Result<()> {
        let mut names = HashSet::new();
        for destination in &self.destinations {
            destination.validate()?;
            if !names.insert(destination.name.clone()) {
                return Err(plugin_err(format!(
                    "duplicate backup destination '{}'",
                    destination.name
                )));
            }
        }
        Ok(())
    }
}

impl Default for BackupDestinationsConfig {
    fn default() -> Self {
        Self {
            verify_after_upload: default_verify_after_upload(),
            block_on_remote_failure: default_block_on_remote_failure(),
            rclone_path: default_rclone_path(),
            destinations: Vec::new(),
        }
    }
}

fn default_verify_after_upload() -> bool {
    true
}

fn default_block_on_remote_failure() -> bool {
    true
}

fn default_rclone_path() -> String {
    "rclone".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_destination_names() {
        let config = BackupDestinationsConfig {
            destinations: vec![
                BackupDestinationConfig::rclone("offsite", "b2:voom"),
                BackupDestinationConfig::rclone("offsite", "s3:voom"),
            ],
            ..BackupDestinationsConfig::default()
        };

        let err = config.validate().unwrap_err();

        assert!(err.to_string().contains("duplicate backup destination"));
    }

    #[test]
    fn rejects_rclone_destination_without_remote() {
        let config = BackupDestinationsConfig {
            destinations: vec![BackupDestinationConfig {
                name: "offsite".to_string(),
                kind: DestinationKind::Rclone,
                remote: None,
                bandwidth_limit: None,
            }],
            ..BackupDestinationsConfig::default()
        };

        let err = config.validate().unwrap_err();

        assert!(err.to_string().contains("requires remote"));
    }

    #[test]
    fn accepts_typed_rclone_backed_destinations() {
        let config = BackupDestinationsConfig {
            destinations: vec![
                BackupDestinationConfig {
                    name: "archive-s3".to_string(),
                    kind: DestinationKind::S3,
                    remote: Some("aws:voom".to_string()),
                    bandwidth_limit: None,
                },
                BackupDestinationConfig {
                    name: "nas-sftp".to_string(),
                    kind: DestinationKind::Sftp,
                    remote: Some("vps:/srv/voom".to_string()),
                    bandwidth_limit: Some("10M".to_string()),
                },
                BackupDestinationConfig {
                    name: "webdav".to_string(),
                    kind: DestinationKind::Webdav,
                    remote: Some("dav:voom".to_string()),
                    bandwidth_limit: None,
                },
            ],
            ..BackupDestinationsConfig::default()
        };

        config.validate().unwrap();
    }
}
