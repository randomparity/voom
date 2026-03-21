use chrono::{DateTime, Utc};

/// Format a datetime for display in human-readable form.
#[must_use]
pub fn format_display(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// Format a datetime as ISO 8601 (for serialization/API responses).
#[must_use]
pub fn format_iso(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// Format a duration in seconds to a human-readable string (e.g., "1h 23m 45s").
#[must_use]
pub fn format_duration(seconds: f64) -> String {
    #[allow(clippy::cast_sign_loss)] // negative durations aren't meaningful
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Format a file size in bytes to human-readable form.
#[must_use]
#[allow(clippy::cast_precision_loss)] // precision loss is negligible for display formatting
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(45.0), "45s");
        assert_eq!(format_duration(90.0), "1m 30s");
        assert_eq!(format_duration(3661.0), "1h 01m 01s");
        assert_eq!(format_duration(7200.0), "2h 00m 00s");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1 KiB");
        assert_eq!(format_size(1_048_576), "1.0 MiB");
        assert_eq!(format_size(1_073_741_824), "1.00 GiB");
        assert_eq!(format_size(2_500_000_000), "2.33 GiB");
    }

    #[test]
    fn test_format_display() {
        let dt = DateTime::parse_from_rfc3339("2025-06-15T12:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_display(&dt), "2025-06-15 12:30:00 UTC");
    }

    #[test]
    fn test_format_iso() {
        let dt = DateTime::parse_from_rfc3339("2025-06-15T12:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let iso = format_iso(&dt);
        assert!(iso.contains("2025-06-15"));
    }
}
