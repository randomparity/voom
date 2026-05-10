//! Remote backup destination configuration and command helpers.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
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

/// A remote backup location that received a copy of the source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBackupRecord {
    pub destination_name: String,
    pub remote_path: String,
    pub verified: bool,
}

/// Input passed to a remote upload runner.
#[derive(Debug, Clone)]
pub struct RemoteUploadRequest<'a> {
    pub destination: &'a BackupDestinationConfig,
    pub source_path: &'a Path,
    pub remote_path: String,
    pub expected_size: u64,
    pub rclone_path: &'a str,
    pub verify_after_upload: bool,
}

/// Result returned by a remote upload runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteUploadReceipt {
    pub verified: bool,
}

/// Command description used for testable rclone argv construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcloneCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl RcloneCommand {
    #[must_use]
    pub fn copyto(
        program: impl Into<String>,
        source: &Path,
        remote_path: &str,
        bandwidth_limit: Option<&str>,
    ) -> Self {
        let mut args = vec![
            "copyto".to_string(),
            source.display().to_string(),
            remote_path.to_string(),
        ];
        if let Some(limit) = bandwidth_limit {
            args.push("--bwlimit".to_string());
            args.push(limit.to_string());
        }
        Self {
            program: program.into(),
            args,
        }
    }

    #[must_use]
    pub fn size(program: impl Into<String>, remote_path: &str) -> Self {
        Self {
            program: program.into(),
            args: vec![
                "size".to_string(),
                "--json".to_string(),
                remote_path.to_string(),
            ],
        }
    }

    fn run(&self) -> Result<Vec<u8>> {
        let output = Command::new(&self.program)
            .args(&self.args)
            .output()
            .map_err(|e| plugin_err(format!("failed to run {}: {e}", self.program)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(plugin_err(format!(
                "{} failed with status {}: {}",
                self.program,
                output.status,
                stderr.trim()
            )));
        }
        Ok(output.stdout)
    }
}

#[derive(Deserialize)]
struct RcloneSizeOutput {
    bytes: u64,
}

pub fn remote_path_for(
    destination: &BackupDestinationConfig,
    id: Uuid,
    source: &Path,
) -> Result<String> {
    let remote = destination.remote.as_ref().ok_or_else(|| {
        plugin_err(format!(
            "backup destination '{}' requires remote",
            destination.name
        ))
    })?;
    let file_name = source.file_name().map_or_else(
        || "unknown".to_string(),
        |name| name.to_string_lossy().replace(['/', '\\', '\0'], "_"),
    );
    Ok(format!(
        "{}/{id}/{file_name}.vbak",
        remote.trim_end_matches('/')
    ))
}

pub fn upload_with_rclone(request: RemoteUploadRequest<'_>) -> Result<RemoteUploadReceipt> {
    let copy = RcloneCommand::copyto(
        request.rclone_path,
        request.source_path,
        &request.remote_path,
        request.destination.bandwidth_limit.as_deref(),
    );
    copy.run()?;

    if request.verify_after_upload {
        verify_remote_size(
            request.rclone_path,
            &request.remote_path,
            request.expected_size,
        )?;
    }

    Ok(RemoteUploadReceipt {
        verified: request.verify_after_upload,
    })
}

fn verify_remote_size(rclone_path: &str, remote_path: &str, expected_size: u64) -> Result<()> {
    let command = RcloneCommand::size(rclone_path, remote_path);
    let stdout = command.run()?;
    let size: RcloneSizeOutput = serde_json::from_slice(&stdout).map_err(|e| {
        plugin_err(format!(
            "failed to parse rclone size output for {remote_path}: {e}"
        ))
    })?;
    if size.bytes != expected_size {
        return Err(plugin_err(format!(
            "remote backup size mismatch for {remote_path}: expected {expected_size}, got {}",
            size.bytes
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    #[test]
    fn rclone_copyto_uses_argv_without_shell() {
        let command = RcloneCommand::copyto(
            "rclone",
            Path::new("/tmp/movie.mkv"),
            "b2:voom/backup.vbak",
            Some("10M"),
        );

        assert_eq!(command.program, "rclone");
        assert_eq!(command.args[0], "copyto");
        assert_eq!(command.args[1], "/tmp/movie.mkv");
        assert_eq!(command.args[2], "b2:voom/backup.vbak");
        assert!(command.args.contains(&"--bwlimit".to_string()));
    }

    #[test]
    fn remote_path_uses_uuid_and_sanitized_file_name() {
        let destination = BackupDestinationConfig::rclone("offsite", "b2:voom/");
        let id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();

        let path = remote_path_for(&destination, id, &PathBuf::from("Movie.mkv")).unwrap();

        assert_eq!(
            path,
            "b2:voom/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa/Movie.mkv.vbak"
        );
    }
}
