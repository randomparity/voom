use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A file that failed introspection or could not be processed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BadFile {
    pub id: Uuid,
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: Option<String>,
    pub error: String,
    pub error_source: BadFileSource,
    pub attempt_count: u32,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

impl BadFile {
    /// Create a new `BadFile` with the given path and error details.
    #[must_use]
    pub fn new(
        path: PathBuf,
        size: u64,
        content_hash: Option<String>,
        error: String,
        error_source: BadFileSource,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            path,
            size,
            content_hash,
            error,
            error_source,
            attempt_count: 1,
            first_seen_at: now,
            last_seen_at: now,
        }
    }
}

/// The source of a bad file error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadFileSource {
    Introspection,
    Io,
    Parse,
}

impl std::fmt::Display for BadFileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BadFileSource::Introspection => write!(f, "introspection"),
            BadFileSource::Io => write!(f, "io"),
            BadFileSource::Parse => write!(f, "parse"),
        }
    }
}

impl std::str::FromStr for BadFileSource {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "introspection" => Ok(BadFileSource::Introspection),
            "io" => Ok(BadFileSource::Io),
            "parse" => Ok(BadFileSource::Parse),
            _ => Err(format!("unknown bad file source: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_bad_file_with_defaults() {
        let bf = BadFile::new(
            PathBuf::from("/test/bad.mkv"),
            1024,
            Some("abc123".into()),
            "ffprobe failed".into(),
            BadFileSource::Introspection,
        );
        assert_eq!(bf.path, PathBuf::from("/test/bad.mkv"));
        assert_eq!(bf.size, 1024);
        assert_eq!(bf.content_hash, Some("abc123".into()));
        assert_eq!(bf.error, "ffprobe failed");
        assert_eq!(bf.error_source, BadFileSource::Introspection);
        assert_eq!(bf.attempt_count, 1);
        assert_eq!(bf.first_seen_at, bf.last_seen_at);
    }

    #[test]
    fn json_roundtrip() {
        let bf = BadFile::new(
            PathBuf::from("/test/bad.mkv"),
            2048,
            None,
            "corrupt header".into(),
            BadFileSource::Parse,
        );
        let json = serde_json::to_string(&bf).unwrap();
        let deserialized: BadFile = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.path, bf.path);
        assert_eq!(deserialized.error, bf.error);
        assert_eq!(deserialized.error_source, BadFileSource::Parse);
    }

    #[test]
    fn msgpack_roundtrip() {
        let bf = BadFile::new(
            PathBuf::from("/test/bad.avi"),
            512,
            Some("def456".into()),
            "io error".into(),
            BadFileSource::Io,
        );
        let bytes = rmp_serde::to_vec(&bf).unwrap();
        let deserialized: BadFile = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.path, bf.path);
        assert_eq!(deserialized.error_source, BadFileSource::Io);
    }

    #[test]
    fn bad_file_source_display() {
        assert_eq!(BadFileSource::Introspection.to_string(), "introspection");
        assert_eq!(BadFileSource::Io.to_string(), "io");
        assert_eq!(BadFileSource::Parse.to_string(), "parse");
    }

    #[test]
    fn bad_file_source_from_str() {
        assert_eq!(
            "introspection".parse::<BadFileSource>().unwrap(),
            BadFileSource::Introspection
        );
        assert_eq!("io".parse::<BadFileSource>().unwrap(), BadFileSource::Io);
        assert_eq!(
            "parse".parse::<BadFileSource>().unwrap(),
            BadFileSource::Parse
        );
        assert!("unknown".parse::<BadFileSource>().is_err());
    }
}
