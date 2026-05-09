//! Parse `--since` arguments — relative durations or absolute datetimes.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

/// Parse `30d` / `4w` / `12h` / `2026-01-15` / `2026-01-15T10:30:00`
/// into a UTC `DateTime`. Relative forms are subtracted from "now".
///
/// # Errors
/// Returns an error if the input doesn't match any supported format.
pub fn parse_since(s: &str) -> Result<DateTime<Utc>> {
    voom_domain::utils::since::parse_since(s).map_err(|e| anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_days() {
        let dt = parse_since("30d").unwrap();
        let diff = (Utc::now() - dt).num_days();
        assert!((29..=31).contains(&diff));
    }

    #[test]
    fn parses_weeks() {
        let dt = parse_since("2w").unwrap();
        let diff = (Utc::now() - dt).num_days();
        assert!((13..=15).contains(&diff));
    }

    #[test]
    fn parses_hours() {
        let dt = parse_since("12h").unwrap();
        let diff = (Utc::now() - dt).num_hours();
        assert!((11..=13).contains(&diff));
    }

    #[test]
    fn parses_absolute_date() {
        let dt = parse_since("2026-01-15").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-01-15T00:00:00+00:00");
    }

    #[test]
    fn parses_absolute_datetime() {
        let dt = parse_since("2026-01-15T10:30:00").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-01-15T10:30:00+00:00");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_since("nonsense").is_err());
        assert!(parse_since("").is_err());
        assert!(parse_since("d30").is_err()); // unit before number
    }
}
