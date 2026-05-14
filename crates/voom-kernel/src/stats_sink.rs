//! Stats sink for plugin invocation records.
//!
//! The event bus dispatcher wraps each plugin handler invocation in a timer
//! and forwards a [`PluginStatRecord`] to the configured sink. The default
//! sink is [`NoopStatsSink`], which discards all records — used in tests and
//! when stats collection is disabled.
//!
//! Production sinks (e.g. `SqliteStatsSink` in `plugins/sqlite-store`) must
//! be non-blocking: `record` is called inside the bus dispatch loop, which
//! holds the subscriber lock. A bounded channel + writer thread is the
//! expected pattern.

use voom_domain::plugin_stats::PluginStatRecord;

pub trait StatsSink: Send + Sync {
    fn record(&self, record: PluginStatRecord);
}

/// Sink that discards all records. Used when stats collection is disabled.
#[derive(Default)]
pub struct NoopStatsSink;

impl StatsSink for NoopStatsSink {
    fn record(&self, _record: PluginStatRecord) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use voom_domain::plugin_stats::PluginInvocationOutcome;

    #[test]
    fn noop_sink_does_not_panic() {
        let sink = NoopStatsSink;
        sink.record(PluginStatRecord::new(
            "x",
            "y",
            Utc::now(),
            0,
            PluginInvocationOutcome::Ok,
        ));
    }
}
