//! Hash mode: streaming sha256 for bit-rot detection.
//!
//! Computes sha256 of the full file. Compared against the previous hash
//! record for the same file (passed in by the caller) to detect bit-rot.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::verification::{
    VerificationMode, VerificationOutcome, VerificationRecord, VerificationRecordInput,
};

/// Run hash verification on `path`. `prior` is the previous hash record
/// for the same file (used to detect bit-rot). `None` means this is the
/// first hash run — outcome is always `Ok` and the new hash becomes the
/// baseline.
///
/// # Errors
/// Returns an error if the file cannot be opened or read.
pub fn run_hash(
    file_id: &str,
    path: &Path,
    prior: Option<&VerificationRecord>,
) -> Result<VerificationRecord> {
    let started = Utc::now();
    let hash = compute_sha256(path)?;
    let outcome = match prior.and_then(|r| r.content_hash.as_deref()) {
        Some(prior_hash) if prior_hash != hash => VerificationOutcome::Error,
        _ => VerificationOutcome::Ok,
    };
    let details = if outcome == VerificationOutcome::Error {
        Some(
            serde_json::json!({
                "prior_hash": prior.and_then(|r| r.content_hash.clone()),
                "prior_at": prior.map(|r| r.verified_at.to_rfc3339()),
            })
            .to_string(),
        )
    } else {
        None
    };
    let error_count = u32::from(outcome == VerificationOutcome::Error);
    Ok(VerificationRecord::new(VerificationRecordInput {
        id: Uuid::new_v4(),
        file_id: file_id.to_string(),
        verified_at: started,
        mode: VerificationMode::Hash,
        outcome,
        error_count,
        warning_count: 0,
        content_hash: Some(hash),
        details,
    }))
}

fn compute_sha256(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|e| VoomError::ToolExecution {
        tool: "sha256".into(),
        message: format!("open {}: {e}", path.display()),
    })?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| VoomError::ToolExecution {
                tool: "sha256".into(),
                message: format!("read {}: {e}", path.display()),
            })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(hex_encode(digest.as_slice()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn first_hash_is_ok_and_records_baseline() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"hello").unwrap();
        let rec = run_hash("file-id", tmp.path(), None).unwrap();
        assert_eq!(rec.outcome, VerificationOutcome::Ok);
        assert!(rec.content_hash.is_some());
    }

    #[test]
    fn matching_hash_is_ok() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"stable").unwrap();
        let first = run_hash("file-id", tmp.path(), None).unwrap();
        let second = run_hash("file-id", tmp.path(), Some(&first)).unwrap();
        assert_eq!(second.outcome, VerificationOutcome::Ok);
        assert_eq!(first.content_hash, second.content_hash);
    }

    #[test]
    fn mismatched_hash_is_error() {
        let prior = VerificationRecord::new(VerificationRecordInput {
            id: Uuid::new_v4(),
            file_id: "file-id".into(),
            verified_at: Utc::now(),
            mode: VerificationMode::Hash,
            outcome: VerificationOutcome::Ok,
            error_count: 0,
            warning_count: 0,
            content_hash: Some("deadbeef".into()),
            details: None,
        });
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"different content").unwrap();
        let rec = run_hash("file-id", tmp.path(), Some(&prior)).unwrap();
        assert_eq!(rec.outcome, VerificationOutcome::Error);
        assert_eq!(rec.error_count, 1);
    }

    #[test]
    fn missing_file_errors() {
        let r = run_hash("file-id", Path::new("/nonexistent/file"), None);
        assert!(r.is_err());
    }
}
