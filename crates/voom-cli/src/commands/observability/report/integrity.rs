use std::io::Write as _;

use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;

use crate::cli::OutputFormat;
use crate::output;
use voom_domain::verification::IntegritySummary;

// ── Integrity summary ──────────────────────────────────────

const INTEGRITY_STALE_DAYS: i64 = 30;

pub(super) fn run(
    store: &dyn voom_domain::storage::StorageTrait,
    format: OutputFormat,
) -> Result<()> {
    let since_cutoff = chrono::Utc::now() - chrono::Duration::days(INTEGRITY_STALE_DAYS);
    let summary = store
        .integrity_summary(since_cutoff)
        .context("failed to query integrity summary")?;

    match format {
        OutputFormat::Json => print_integrity_json(&summary, since_cutoff)?,
        OutputFormat::Table => print_integrity_table(&summary, since_cutoff),
        OutputFormat::Plain => print_integrity_plain(&summary),
        OutputFormat::Csv => print_integrity_csv(&summary)?,
    }
    Ok(())
}

fn print_integrity_table(summary: &IntegritySummary, since_cutoff: chrono::DateTime<chrono::Utc>) {
    println!(
        "{} (stale cutoff: {})",
        style("Library Integrity").bold().underlined(),
        since_cutoff.format("%Y-%m-%d"),
    );
    println!();
    let mut table = output::new_table();
    table.set_header(vec!["Metric", "Count"]);
    table.add_row(vec![
        Cell::new("Total files"),
        Cell::new(summary.total_files),
    ]);
    table.add_row(vec![
        Cell::new("Never verified"),
        Cell::new(summary.never_verified),
    ]);
    table.add_row(vec![
        Cell::new(format!("Stale (> {INTEGRITY_STALE_DAYS}d)")),
        Cell::new(summary.stale),
    ]);
    table.add_row(vec![
        Cell::new("With errors"),
        Cell::new(summary.with_errors),
    ]);
    table.add_row(vec![
        Cell::new("With warnings"),
        Cell::new(summary.with_warnings),
    ]);
    table.add_row(vec![
        Cell::new("Hash mismatches"),
        Cell::new(summary.hash_mismatches),
    ]);
    println!("{table}");
    println!();
}

fn print_integrity_plain(summary: &IntegritySummary) {
    println!("total_files={}", summary.total_files);
    println!("never_verified={}", summary.never_verified);
    println!("stale={}", summary.stale);
    println!("with_errors={}", summary.with_errors);
    println!("with_warnings={}", summary.with_warnings);
    println!("hash_mismatches={}", summary.hash_mismatches);
}

fn print_integrity_json(
    summary: &IntegritySummary,
    since_cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let payload = serde_json::json!({
        "stale_cutoff": since_cutoff.to_rfc3339(),
        "stale_cutoff_days": INTEGRITY_STALE_DAYS,
        "summary": summary,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).context("serialize integrity summary")?
    );
    Ok(())
}

fn print_integrity_csv(summary: &IntegritySummary) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# integrity")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "total_files",
        "never_verified",
        "stale",
        "with_errors",
        "with_warnings",
        "hash_mismatches",
    ])?;
    wtr.write_record([
        summary.total_files.to_string(),
        summary.never_verified.to_string(),
        summary.stale.to_string(),
        summary.with_errors.to_string(),
        summary.with_warnings.to_string(),
        summary.hash_mismatches.to_string(),
    ])?;
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use voom_domain::test_support::InMemoryStore;

    use super::*;

    #[test]
    fn test_run_integrity_on_empty_store() {
        let store = InMemoryStore::new();
        // All formats must succeed against an empty store; integrity_summary
        // returns the default (all zeros) so no rows are missing.
        for format in [
            OutputFormat::Table,
            OutputFormat::Plain,
            OutputFormat::Json,
            OutputFormat::Csv,
        ] {
            run(&store, format).expect("integrity summary on empty store");
        }
    }
}
