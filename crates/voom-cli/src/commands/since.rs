//! Parse `--since` arguments — relative durations or absolute datetimes.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// Parse `30d` / `4w` / `12h` / `2026-01-15` / `2026-01-15T10:30:00`
/// into a UTC `DateTime`. Relative forms are subtracted from "now".
///
/// # Errors
/// Returns an error if the input doesn't match any supported format.
pub fn parse_since(s: &str) -> Result<DateTime<Utc>> {
    if let Some(dt) = parse_relative(s) {
        return Ok(dt);
    }
    parse_absolute_since(s)
        .map_err(|_| anyhow!("invalid --since '{s}': expected `30d`, `4w`, `12h`, or YYYY-MM-DD"))
}

/// Parse `2026-01-15` / `2026-01-15T10:30:00` into a UTC `DateTime`.
///
/// # Errors
/// Returns an error if the input is not an absolute date or datetime.
pub fn parse_absolute_since(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = nd.and_hms_opt(0, 0, 0).expect("midnight is always valid");
        return Ok(Utc.from_utc_datetime(&ndt));
    }
    Err(anyhow!(
        "invalid datetime '{s}': expected YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS"
    ))
}

fn parse_relative(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    let split_at = s.find(char::is_alphabetic)?;
    if split_at == 0 {
        return None;
    }
    let (num_part, unit) = s.split_at(split_at);
    let n: i64 = num_part.trim().parse().ok()?;
    let dur = match unit {
        "h" | "hr" | "hours" => Duration::hours(n),
        "d" | "day" | "days" => Duration::days(n),
        "w" | "week" | "weeks" => Duration::days(n * 7),
        _ => return None,
    };
    Some(Utc::now() - dur)
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
    fn absolute_parser_rejects_relative_since() {
        assert!(parse_absolute_since("30d").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_since("nonsense").is_err());
        assert!(parse_since("").is_err());
        assert!(parse_since("d30").is_err()); // unit before number
    }
}
