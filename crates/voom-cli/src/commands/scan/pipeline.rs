//! Streaming scan pipeline: discovery → ingest → bounded probe pool.

/// Default SQLite connection pool size. Mirrors
/// `voom_sqlite_store::store::SqliteStoreConfig::default().pool_size`.
/// Hard-coded here to avoid a circular dep on the store crate's config type.
// TODO(#359): remove once wired into the streaming pipeline (Task 6)
#[allow(dead_code)]
const DEFAULT_SQLITE_POOL: usize = 8;

/// Slots we reserve out of the pool for non-probe work (one writer reacting
/// to events, one ad-hoc reader). Anything below this is unsafe — probes
/// would deadlock against the bus.
// TODO(#359): remove once wired into the streaming pipeline (Task 6)
#[allow(dead_code)]
const RESERVED_POOL_SLOTS: usize = 2;

/// Pick the introspection worker count when `--probe-workers` is `0`.
///
/// Returns `min(num_cpus, pool_size - reserved)` with a floor of `1`.
// TODO(#359): remove once wired into the streaming pipeline (Task 6)
#[allow(dead_code)]
#[must_use]
pub(crate) fn auto_probe_workers(num_cpus: usize, pool_size: usize) -> usize {
    let cap = pool_size.saturating_sub(RESERVED_POOL_SLOTS);
    num_cpus.min(cap).max(1)
}

/// Resolve the effective probe worker count from the CLI flag.
// TODO(#359): remove once wired into the streaming pipeline (Task 6)
#[allow(dead_code)]
#[must_use]
pub(crate) fn resolve_probe_workers(flag: usize) -> usize {
    if flag == 0 {
        auto_probe_workers(num_cpus::get(), DEFAULT_SQLITE_POOL)
    } else {
        flag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_probe_workers_caps_at_pool_minus_reserved() {
        assert_eq!(auto_probe_workers(16, 8), 6);
    }

    #[test]
    fn auto_probe_workers_uses_cpus_when_pool_is_generous() {
        assert_eq!(auto_probe_workers(4, 32), 4);
    }

    #[test]
    fn auto_probe_workers_floor_is_one() {
        assert_eq!(auto_probe_workers(0, 0), 1);
        assert_eq!(auto_probe_workers(1, 2), 1);
        assert_eq!(auto_probe_workers(1, 1), 1);
    }

    #[test]
    fn resolve_probe_workers_explicit_flag_wins() {
        assert_eq!(resolve_probe_workers(3), 3);
        assert_eq!(resolve_probe_workers(1), 1);
    }
}
