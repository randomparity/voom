//! `voom plugin stats` — render a per-plugin invocation rollup with
//! p50/p95/p99 latencies and outcome counts.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, Utc};
use voom_domain::plugin_stats::{PluginStatsFilter, PluginStatsRollup};

use crate::app;
use crate::cli::OutputFormat;
use crate::config;

pub fn run(
    plugin: Option<String>,
    since: Option<String>,
    top: Option<usize>,
    format: OutputFormat,
) -> Result<()> {
    let since_dt = since.as_deref().map(parse_since).transpose()?;
    let cfg = config::load_config()?;
    // Read-only path: open the SQLite pool directly. Do NOT construct a
    // kernel or install a stats sink — that would write to event_log and
    // plugin_stats just by running this command. See Codex adversarial
    // review (May 2026).
    let store = app::open_store(&cfg)?;
    let filter = PluginStatsFilter::new(plugin, since_dt, top);
    let rollups = store.rollup_plugin_stats(&filter)?;
    render(&rollups, format)
}

fn parse_since(s: &str) -> Result<DateTime<Utc>> {
    // Accept "24h", "7d", "30m" — number + unit (s/m/h/d).
    // Split on a char boundary so multi-byte trailing chars don't panic.
    let split = s.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
    let (num_str, unit) = s.split_at(split);
    let n: i64 = num_str
        .parse()
        .map_err(|_| anyhow!("invalid --since value: {s}"))?;
    if n <= 0 {
        return Err(anyhow!("--since must be a positive duration, got: {s}"));
    }
    let d = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        _ => return Err(anyhow!("invalid --since unit '{unit}'; expected s|m|h|d")),
    };
    Ok(Utc::now() - d)
}

fn render(rollups: &[PluginStatsRollup], format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(rollups)?;
            println!("{json}");
            Ok(())
        }
        OutputFormat::Table => render_table(rollups),
        OutputFormat::Plain | OutputFormat::Csv => Err(anyhow!(
            "--format {format:?} is not supported for `voom plugin stats`; use table or json"
        )),
    }
}

fn render_table(rollups: &[PluginStatsRollup]) -> Result<()> {
    if rollups.is_empty() {
        println!("No plugin invocations recorded.");
        return Ok(());
    }
    println!(
        "{:<28} {:>8} {:>6} {:>8} {:>6} {:>6} {:>8} {:>8} {:>8}",
        "plugin", "calls", "ok", "skipped", "err", "panic", "p50ms", "p95ms", "p99ms"
    );
    for r in rollups {
        println!(
            "{:<28} {:>8} {:>6} {:>8} {:>6} {:>6} {:>8} {:>8} {:>8}",
            r.plugin_id,
            r.invocation_count,
            r.ok_count,
            r.skipped_count,
            r.err_count,
            r.panic_count,
            r.p50_ms,
            r.p95_ms,
            r.p99_ms
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_hours() {
        let dt = parse_since("24h").unwrap();
        let diff = Utc::now() - dt;
        assert!(diff.num_hours() >= 23 && diff.num_hours() <= 25);
    }

    #[test]
    fn parse_since_days() {
        let dt = parse_since("7d").unwrap();
        let diff = Utc::now() - dt;
        assert!(diff.num_days() >= 6 && diff.num_days() <= 8);
    }

    #[test]
    fn parse_since_rejects_bad_unit() {
        assert!(parse_since("24x").is_err());
    }

    #[test]
    fn parse_since_rejects_non_numeric() {
        assert!(parse_since("abh").is_err());
    }

    #[test]
    fn parse_since_rejects_negative() {
        assert!(parse_since("-1h").is_err());
    }

    #[test]
    fn parse_since_rejects_zero() {
        assert!(parse_since("0h").is_err());
    }

    #[test]
    fn parse_since_multibyte_does_not_panic() {
        // Returns Err because "⏱" is not a valid unit; the important property
        // is that we do not panic on the byte split.
        let r = parse_since("24⏱");
        assert!(r.is_err());
    }
}
