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

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Rclone => "rclone",
            Self::S3 => "s3",
            Self::Sftp => "sftp",
            Self::Webdav => "webdav",
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
    #[serde(default)]
    pub minimum_storage_days: Option<u32>,
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
            minimum_storage_days: None,
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

    #[must_use]
    pub fn hashsum(program: impl Into<String>, algorithm: &str, remote_path: &str) -> Self {
        Self {
            program: program.into(),
            args: vec![
                "hashsum".to_string(),
                algorithm.to_string(),
                remote_path.to_string(),
            ],
        }
    }

    #[must_use]
    pub fn deletefile(program: impl Into<String>, remote_path: &str) -> Self {
        Self {
            program: program.into(),
            args: vec!["deletefile".to_string(), remote_path.to_string()],
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

pub fn download_with_rclone(
    rclone_path: &str,
    remote_path: &str,
    output_path: &Path,
    expected_size: u64,
) -> Result<()> {
    verify_remote_size(rclone_path, remote_path, expected_size)?;
    let copy = RcloneCommand::copyto(
        rclone_path,
        Path::new(remote_path),
        &output_path.display().to_string(),
        None,
    );
    copy.run()?;
    let downloaded = std::fs::metadata(output_path)
        .map_err(|e| {
            plugin_err(format!(
                "failed to read restored backup {}: {e}",
                output_path.display()
            ))
        })?
        .len();
    if downloaded != expected_size {
        return Err(plugin_err(format!(
            "restored backup size mismatch for {}: expected {expected_size}, got {downloaded}",
            output_path.display()
        )));
    }
    Ok(())
}

fn verify_remote_size(rclone_path: &str, remote_path: &str, expected_size: u64) -> Result<()> {
    let actual_size = remote_size(rclone_path, remote_path)?;
    if actual_size != expected_size {
        return Err(plugin_err(format!(
            "remote backup size mismatch for {remote_path}: expected {expected_size}, got {actual_size}",
        )));
    }
    Ok(())
}

pub fn remote_size(rclone_path: &str, remote_path: &str) -> Result<u64> {
    let command = RcloneCommand::size(rclone_path, remote_path);
    let stdout = command.run()?;
    let size: RcloneSizeOutput = serde_json::from_slice(&stdout).map_err(|e| {
        plugin_err(format!(
            "failed to parse rclone size output for {remote_path}: {e}"
        ))
    })?;
    Ok(size.bytes)
}

pub fn remote_sha256(rclone_path: &str, remote_path: &str) -> Result<Option<String>> {
    let command = RcloneCommand::hashsum(rclone_path, "SHA-256", remote_path);
    let stdout = match command.run() {
        Ok(stdout) => stdout,
        Err(_) => return Ok(None),
    };
    let text = String::from_utf8_lossy(&stdout);
    let Some(hash) = text.split_whitespace().next() else {
        return Ok(None);
    };
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(hash.to_ascii_lowercase()))
    } else {
        Ok(None)
    }
}

pub fn delete_with_rclone(rclone_path: &str, remote_path: &str) -> Result<()> {
    RcloneCommand::deletefile(rclone_path, remote_path).run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    fn make_executable(path: &Path) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

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
                minimum_storage_days: None,
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
                    minimum_storage_days: Some(180),
                },
                BackupDestinationConfig {
                    name: "nas-sftp".to_string(),
                    kind: DestinationKind::Sftp,
                    remote: Some("vps:/srv/voom".to_string()),
                    bandwidth_limit: Some("10M".to_string()),
                    minimum_storage_days: None,
                },
                BackupDestinationConfig {
                    name: "webdav".to_string(),
                    kind: DestinationKind::Webdav,
                    remote: Some("dav:voom".to_string()),
                    bandwidth_limit: None,
                    minimum_storage_days: None,
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
    fn rclone_hashsum_uses_sha256_without_shell() {
        let command = RcloneCommand::hashsum("rclone", "SHA-256", "b2:voom/backup.vbak");

        assert_eq!(command.program, "rclone");
        assert_eq!(
            command.args,
            vec!["hashsum", "SHA-256", "b2:voom/backup.vbak"]
        );
    }

    #[test]
    fn rclone_deletefile_uses_argv_without_shell() {
        let command = RcloneCommand::deletefile("rclone", "b2:voom/backup.vbak");

        assert_eq!(command.program, "rclone");
        assert_eq!(command.args, vec!["deletefile", "b2:voom/backup.vbak"]);
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

    #[test]
    fn download_with_rclone_verifies_size_and_writes_output() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = dir.path().join("fake-rclone");
        let remote_root = dir.path().join("remote");
        let output = dir.path().join("restored.mkv");
        let remote_file = remote_root.join("fake:voom/movie.vbak");
        std::fs::create_dir_all(remote_file.parent().unwrap()).unwrap();
        std::fs::write(&remote_file, b"movie").unwrap();
        std::fs::write(
            &rclone,
            format!(
                "#!/usr/bin/env bash\n\
                 set -euo pipefail\n\
                 cmd=\"$1\"\n\
                 shift\n\
                 case \"$cmd\" in\n\
                   size)\n\
                     shift\n\
                     bytes=\"$(wc -c < \"{}/$1\" | tr -d ' ')\"\n\
                     printf '{{\"bytes\":%s,\"count\":1}}\\n' \"$bytes\"\n\
                     ;;\n\
                   copyto)\n\
                     cp \"{}/$1\" \"$2\"\n\
                     ;;\n\
                 esac\n",
                remote_root.display(),
                remote_root.display(),
            ),
        )
        .unwrap();
        make_executable(&rclone);

        download_with_rclone(
            &rclone.display().to_string(),
            "fake:voom/movie.vbak",
            &output,
            5,
        )
        .unwrap();

        assert_eq!(std::fs::read(output).unwrap(), b"movie");
    }
}
