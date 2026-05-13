//! Per-plugin invocation statistics: the unit captured by the event bus
//! dispatcher and persisted to the `plugin_stats` SQLite table.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginInvocationOutcome {
    /// Plugin handled the event and produced an `EventResult`.
    Ok,
    /// Plugin handled the event but produced no result (returned `Ok(None)`).
    /// Mapped from the bus dispatcher's "event acknowledged (no result)" path.
    Skipped,
    /// Plugin returned `Err`. `category` is the low-cardinality variant name
    /// of `VoomError` (e.g. `"storage"`, `"plugin"`).
    Err { category: String },
    /// Plugin panicked. Caught by `catch_unwind` in the dispatcher.
    Panic,
}

impl PluginInvocationOutcome {
    #[must_use]
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Skipped => "skipped",
            Self::Err { .. } => "err",
            Self::Panic => "panic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginStatRecord {
    pub plugin_id: String,
    pub event_type: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub outcome: PluginInvocationOutcome,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginStatsRollup {
    pub plugin_id: String,
    pub invocation_count: u64,
    pub ok_count: u64,
    pub skipped_count: u64,
    pub err_count: u64,
    pub panic_count: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PluginStatsFilter {
    pub plugin: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub top: Option<usize>,
}

/// Nearest-rank percentile (1-indexed rank = ceil(p * n / 100), clamped to [1, n]).
///
/// For `sorted = 1..=100`:
/// - `nearest_rank_percentile(&v, 50)` → `50` (rank 50)
/// - `nearest_rank_percentile(&v, 95)` → `95`
/// - `nearest_rank_percentile(&v, 99)` → `99`
///
/// Returns `0` when the slice is empty. Inputs MUST be sorted ascending.
#[must_use]
pub fn nearest_rank_percentile(sorted: &[u64], p: u64) -> u64 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    let rank = ((p * n as u64).div_ceil(100)).max(1) as usize;
    let idx = rank.min(n) - 1;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_matches_ceil_definition() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(nearest_rank_percentile(&v, 50), 50);
        assert_eq!(nearest_rank_percentile(&v, 95), 95);
        assert_eq!(nearest_rank_percentile(&v, 99), 99);
        assert_eq!(nearest_rank_percentile(&v, 100), 100);
        assert_eq!(nearest_rank_percentile(&v, 1), 1);
    }

    #[test]
    fn nearest_rank_empty_returns_zero() {
        assert_eq!(nearest_rank_percentile(&[], 50), 0);
    }

    #[test]
    fn nearest_rank_single_element() {
        assert_eq!(nearest_rank_percentile(&[42], 50), 42);
        assert_eq!(nearest_rank_percentile(&[42], 99), 42);
    }

    #[test]
    fn outcome_labels_match() {
        assert_eq!(PluginInvocationOutcome::Ok.as_label(), "ok");
        assert_eq!(PluginInvocationOutcome::Skipped.as_label(), "skipped");
        assert_eq!(
            PluginInvocationOutcome::Err {
                category: "io".into()
            }
            .as_label(),
            "err"
        );
        assert_eq!(PluginInvocationOutcome::Panic.as_label(), "panic");
    }

    #[test]
    fn record_roundtrips_via_serde() {
        let r = PluginStatRecord {
            plugin_id: "discovery".into(),
            event_type: "file.discovered".into(),
            started_at: Utc::now(),
            duration_ms: 12,
            outcome: PluginInvocationOutcome::Ok,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: PluginStatRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}
