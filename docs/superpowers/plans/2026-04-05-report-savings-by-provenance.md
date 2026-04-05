# Report: Space Savings by Provenance — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `voom report --savings` flag that surfaces space savings broken down by executor, phase, and time period, using data already stored in the `file_transitions` table.

**Architecture:** Add a new `SavingsQuery` trait method to `FileTransitionStorage` that runs aggregate SQL on `file_transitions` grouped by `source_detail` (executor), `phase_name`, and time bucket. Return a new `SavingsReport` domain type. Render in CLI with table/json/plain output formats.

**Tech Stack:** Rust, rusqlite (aggregate SQL), comfy-table (CLI rendering), serde_json (JSON output), chrono (time bucketing)

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/voom-domain/src/storage.rs` | Modify | Add `savings_by_provenance()` method to `FileTransitionStorage` trait |
| `crates/voom-domain/src/stats.rs` | Modify | Add `SavingsReport` and `SavingsBucket` types |
| `plugins/sqlite-store/src/store/file_transition_storage.rs` | Modify | Implement the aggregate SQL query |
| `crates/voom-cli/src/cli.rs` | Modify | Add `--savings` flag to `ReportArgs` |
| `crates/voom-cli/src/commands/report.rs` | Modify | Add `run_savings_report()` with table/json/plain rendering |

---

### Task 1: Add `SavingsReport` domain types

**Files:**
- Modify: `crates/voom-domain/src/stats.rs:146` (after `ProcessingAggregateStats`)

- [ ] **Step 1: Add the `SavingsBucket` and `SavingsReport` types**

Add after the `ProcessingAggregateStats` struct (line ~146):

```rust
/// A single row in a savings breakdown — one per (executor, phase) pair.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SavingsBucket {
    /// Executor name (from `source_detail`, e.g. "mkvtoolnix:normalize").
    /// `None` for transitions missing source_detail.
    pub executor: Option<String>,
    /// Phase name (from `phase_name`).
    /// `None` for transitions missing phase_name.
    pub phase: Option<String>,
    /// Time period label (e.g. "2026-04", "2026-W14", "2026-04-05").
    /// `None` when not grouped by time.
    pub period: Option<String>,
    /// Number of successful transitions in this bucket.
    pub transition_count: u64,
    /// Total bytes saved (positive = smaller after, negative = larger after).
    pub bytes_saved: i64,
    /// Total processing duration in milliseconds.
    pub duration_ms: u64,
    /// Total files processed (distinct file_id count).
    pub file_count: u64,
}

/// The granularity for time-based savings grouping.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimePeriod {
    /// Group by calendar day (YYYY-MM-DD).
    Day,
    /// Group by ISO week (YYYY-WNN).
    Week,
    /// Group by calendar month (YYYY-MM).
    #[default]
    Month,
}

impl TimePeriod {
    /// Returns the SQLite strftime format and label prefix for this period.
    #[must_use]
    pub fn sql_format(&self) -> &'static str {
        match self {
            TimePeriod::Day => "%Y-%m-%d",
            TimePeriod::Week => "%Y-W%W",
            TimePeriod::Month => "%Y-%m",
        }
    }

    /// Parse from a CLI string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "day" => Some(TimePeriod::Day),
            "week" => Some(TimePeriod::Week),
            "month" => Some(TimePeriod::Month),
            _ => None,
        }
    }
}

/// Complete savings report containing bucketed breakdowns.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavingsReport {
    /// Breakdown by (executor, phase, period).
    pub buckets: Vec<SavingsBucket>,
    /// Grand total bytes saved across all buckets.
    pub total_bytes_saved: i64,
    /// Grand total transitions across all buckets.
    pub total_transitions: u64,
}
```

- [ ] **Step 2: Run `cargo test -p voom-domain` to verify compilation**

Run: `cargo test -p voom-domain --lib`
Expected: All existing tests pass, no compilation errors.

- [ ] **Step 3: Commit**

```bash
git add crates/voom-domain/src/stats.rs
git commit -m "feat(domain): add SavingsReport and SavingsBucket types for provenance reporting"
```

---

### Task 2: Add `savings_by_provenance()` to `FileTransitionStorage` trait

**Files:**
- Modify: `crates/voom-domain/src/storage.rs:128-139` (the `FileTransitionStorage` trait)

- [ ] **Step 1: Add the import for `TimePeriod`**

At the top of `storage.rs`, add `TimePeriod` to the stats import (line 12):

```rust
use crate::stats::{LibrarySnapshot, SavingsReport, SnapshotTrigger, TimePeriod};
```

- [ ] **Step 2: Add `savings_by_provenance()` to the trait**

Add this method after `transitions_for_path` (line 138), inside the `FileTransitionStorage` trait:

```rust
    /// Aggregate space savings from successful voom transitions, grouped by
    /// executor (`source_detail`), phase (`phase_name`), and optionally by
    /// time period.
    fn savings_by_provenance(
        &self,
        period: Option<TimePeriod>,
    ) -> Result<SavingsReport>;
```

- [ ] **Step 3: Run `cargo check -p voom-domain` to verify the trait compiles**

Run: `cargo check -p voom-domain`
Expected: Passes (the sqlite-store impl will fail, which is expected — we fix that in Task 3).

- [ ] **Step 4: Commit**

```bash
git add crates/voom-domain/src/storage.rs
git commit -m "feat(domain): add savings_by_provenance method to FileTransitionStorage trait"
```

---

### Task 3: Implement the aggregate SQL query in sqlite-store

**Files:**
- Modify: `plugins/sqlite-store/src/store/file_transition_storage.rs`

- [ ] **Step 1: Write a test for `savings_by_provenance` with no transitions**

Add at the bottom of `plugins/sqlite-store/src/store/file_transition_storage.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::test_store;
    use voom_domain::stats::TimePeriod;

    #[test]
    fn savings_by_provenance_empty_db() {
        let store = test_store();
        let report = store.savings_by_provenance(None).unwrap();
        assert!(report.buckets.is_empty());
        assert_eq!(report.total_bytes_saved, 0);
        assert_eq!(report.total_transitions, 0);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p sqlite-store savings_by_provenance_empty_db`
Expected: FAIL — `savings_by_provenance` is not implemented.

- [ ] **Step 3: Implement `savings_by_provenance`**

Add the import at the top of the file (after the existing imports):

```rust
use voom_domain::stats::{SavingsBucket, SavingsReport, TimePeriod};
```

Add this method inside the `impl FileTransitionStorage for SqliteStore` block, after `transitions_for_path`:

```rust
    fn savings_by_provenance(
        &self,
        period: Option<TimePeriod>,
    ) -> Result<SavingsReport> {
        let conn = self.conn()?;

        let (period_col, group_by_period) = match period {
            Some(p) => (
                format!("strftime('{}', created_at)", p.sql_format()),
                true,
            ),
            None => ("NULL".to_string(), false),
        };

        let sql = format!(
            "SELECT source_detail, phase_name, {period_col} AS period, \
                    COUNT(*) AS cnt, \
                    COALESCE(SUM(CASE WHEN from_size IS NOT NULL \
                        THEN from_size - to_size ELSE 0 END), 0) AS saved, \
                    COALESCE(SUM(duration_ms), 0) AS dur, \
                    COUNT(DISTINCT file_id) AS files \
             FROM file_transitions \
             WHERE source = 'voom' AND outcome = 'success' \
             GROUP BY source_detail, phase_name{} \
             ORDER BY saved DESC",
            if group_by_period { ", period" } else { "" },
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(storage_err(
                "failed to prepare savings_by_provenance query",
            ))?;

        let buckets: Vec<SavingsBucket> = stmt
            .query_map([], |row| {
                let executor: Option<String> = row.get("source_detail")?;
                let phase: Option<String> = row.get("phase_name")?;
                let period_val: Option<String> = row.get("period")?;
                let cnt: i64 = row.get("cnt")?;
                let saved: i64 = row.get("saved")?;
                let dur: i64 = row.get("dur")?;
                let files: i64 = row.get("files")?;
                Ok(SavingsBucket {
                    executor: executor.filter(|s| !s.is_empty()),
                    phase: phase.filter(|s| !s.is_empty()),
                    period: period_val.filter(|s| !s.is_empty()),
                    transition_count: cnt as u64,
                    bytes_saved: saved,
                    duration_ms: dur as u64,
                    file_count: files as u64,
                })
            })
            .map_err(storage_err(
                "failed to query savings by provenance",
            ))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err(
                "failed to collect savings by provenance",
            ))?;

        let total_bytes_saved: i64 =
            buckets.iter().map(|b| b.bytes_saved).sum();
        let total_transitions: u64 =
            buckets.iter().map(|b| b.transition_count).sum();

        Ok(SavingsReport {
            buckets,
            total_bytes_saved,
            total_transitions,
        })
    }
```

- [ ] **Step 4: Run the empty-db test**

Run: `cargo test -p sqlite-store savings_by_provenance_empty_db`
Expected: PASS

- [ ] **Step 5: Add a test with transition data**

Add to the `tests` module:

```rust
    #[test]
    fn savings_by_provenance_groups_by_executor_and_phase() {
        use voom_domain::stats::ProcessingOutcome;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file_id = Uuid::new_v4();

        // Transition 1: mkvtoolnix:normalize, 1000 -> 800 (saved 200)
        let t1 = FileTransition::new(
            file_id,
            PathBuf::from("/movies/a.mkv"),
            "hash1".into(),
            800,
            TransitionSource::Voom,
        )
        .with_from(Some("hash0".into()), Some(1000))
        .with_detail("mkvtoolnix:normalize")
        .with_processing(
            100, 2, 1,
            ProcessingOutcome::Success,
            "default", "normalize",
        );
        store.record_transition(&t1).unwrap();

        // Transition 2: ffmpeg:transcode, 5000 -> 3000 (saved 2000)
        let t2 = FileTransition::new(
            file_id,
            PathBuf::from("/movies/a.mkv"),
            "hash2".into(),
            3000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash1".into()), Some(5000))
        .with_detail("ffmpeg:transcode")
        .with_processing(
            500, 1, 3,
            ProcessingOutcome::Success,
            "default", "transcode",
        );
        store.record_transition(&t2).unwrap();

        // Transition 3: failed (should be excluded)
        let t3 = FileTransition::new(
            Uuid::new_v4(),
            PathBuf::from("/movies/b.mkv"),
            "hash3".into(),
            9000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash0".into()), Some(10000))
        .with_detail("ffmpeg:transcode")
        .with_processing(
            50, 1, 1,
            ProcessingOutcome::Failure,
            "default", "transcode",
        );
        store.record_transition(&t3).unwrap();

        let report = store.savings_by_provenance(None).unwrap();
        assert_eq!(report.buckets.len(), 2);
        assert_eq!(report.total_bytes_saved, 2200);
        assert_eq!(report.total_transitions, 2);

        // Sorted by saved DESC, so ffmpeg:transcode first
        let first = &report.buckets[0];
        assert_eq!(first.executor.as_deref(), Some("ffmpeg:transcode"));
        assert_eq!(first.phase.as_deref(), Some("transcode"));
        assert_eq!(first.bytes_saved, 2000);
        assert_eq!(first.transition_count, 1);

        let second = &report.buckets[1];
        assert_eq!(second.executor.as_deref(), Some("mkvtoolnix:normalize"));
        assert_eq!(second.phase.as_deref(), Some("normalize"));
        assert_eq!(second.bytes_saved, 200);
    }
```

- [ ] **Step 6: Run all savings tests**

Run: `cargo test -p sqlite-store savings_by_provenance`
Expected: Both tests PASS

- [ ] **Step 7: Add a test for time-period grouping**

Add to the `tests` module:

```rust
    #[test]
    fn savings_by_provenance_with_time_period() {
        use voom_domain::stats::ProcessingOutcome;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();

        let t1 = FileTransition::new(
            Uuid::new_v4(),
            PathBuf::from("/movies/a.mkv"),
            "hash1".into(),
            800,
            TransitionSource::Voom,
        )
        .with_from(Some("hash0".into()), Some(1000))
        .with_detail("mkvtoolnix:normalize")
        .with_processing(
            100, 2, 1,
            ProcessingOutcome::Success,
            "default", "normalize",
        );
        store.record_transition(&t1).unwrap();

        let report = store
            .savings_by_provenance(Some(TimePeriod::Month))
            .unwrap();

        assert_eq!(report.buckets.len(), 1);
        let bucket = &report.buckets[0];
        assert!(bucket.period.is_some(), "period should be populated");
        // Period should be YYYY-MM format
        let period = bucket.period.as_deref().unwrap();
        assert!(
            period.len() == 7 && period.contains('-'),
            "expected YYYY-MM format, got: {period}",
        );
    }
```

- [ ] **Step 8: Run all savings tests and verify**

Run: `cargo test -p sqlite-store savings_by_provenance`
Expected: All 3 tests PASS

- [ ] **Step 9: Commit**

```bash
git add plugins/sqlite-store/src/store/file_transition_storage.rs
git commit -m "feat(sqlite-store): implement savings_by_provenance aggregate query"
```

---

### Task 4: Add `--savings` CLI flag and rendering

**Files:**
- Modify: `crates/voom-cli/src/cli.rs:307-327` (add `--savings` flag and `--period` option)
- Modify: `crates/voom-cli/src/commands/report.rs` (add rendering)

- [ ] **Step 1: Add the `--savings` and `--period` flags to `ReportArgs`**

In `crates/voom-cli/src/cli.rs`, add these fields to `ReportArgs` (before the closing `}`):

```rust
    /// Show space savings breakdown by executor, phase, and time period
    #[arg(long)]
    pub savings: bool,

    /// Time period for savings grouping: day, week, month (default: none)
    #[arg(long, requires = "savings")]
    pub period: Option<String>,
```

- [ ] **Step 2: Add the savings report dispatch in `report.rs`**

In `crates/voom-cli/src/commands/report.rs`, add the import for `FileTransitionStorage` (line 13):

```rust
use voom_domain::storage::{FileTransitionStorage, PlanStorage, SnapshotStorage};
```

Add the dispatch in the `run()` function, after the `args.plans` check (after line 29):

```rust
    if args.savings {
        return run_savings_report(&*store, &args.format, args.period.as_deref());
    }
```

- [ ] **Step 3: Add the `run_savings_report` function**

Add before the `codec_counts` function (before line 586):

```rust
fn run_savings_report(
    store: &dyn FileTransitionStorage,
    format: &OutputFormat,
    period_str: Option<&str>,
) -> Result<()> {
    let period = match period_str {
        Some(s) => {
            let p = voom_domain::stats::TimePeriod::parse(s)
                .context(format!("invalid period '{s}': expected day, week, or month"))?;
            Some(p)
        }
        None => None,
    };

    let report = store
        .savings_by_provenance(period)
        .context("failed to query savings report")?;

    if report.buckets.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .expect("report is serializable")
                );
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style("No savings data. Run 'voom process' first.").yellow()
        );
        return Ok(());
    }

    match format {
        OutputFormat::Json => print_savings_json(&report),
        OutputFormat::Table => print_savings_table(&report, period.is_some()),
        OutputFormat::Plain => print_savings_plain(&report, period.is_some()),
    }

    Ok(())
}

fn print_savings_json(report: &voom_domain::stats::SavingsReport) {
    println!(
        "{}",
        serde_json::to_string_pretty(report)
            .expect("report is serializable")
    );
}

fn print_savings_table(
    report: &voom_domain::stats::SavingsReport,
    show_period: bool,
) {
    use voom_domain::utils::format::{format_duration, format_size};

    println!("{}", style("Space Savings by Provenance").bold().underlined());
    println!();

    let mut table = output::new_table();
    let mut headers: Vec<&str> = vec!["Executor", "Phase"];
    if show_period {
        headers.push("Period");
    }
    headers.extend_from_slice(&["Files", "Transitions", "Saved", "Time"]);
    table.set_header(headers);

    for b in &report.buckets {
        let saved_label = if b.bytes_saved >= 0 {
            format_size(b.bytes_saved as u64)
        } else {
            format!("+{}", format_size(b.bytes_saved.unsigned_abs()))
        };

        let mut row: Vec<Cell> = vec![
            Cell::new(b.executor.as_deref().unwrap_or("-")),
            Cell::new(b.phase.as_deref().unwrap_or("-")),
        ];
        if show_period {
            row.push(Cell::new(b.period.as_deref().unwrap_or("-")));
        }
        row.extend_from_slice(&[
            Cell::new(b.file_count),
            Cell::new(b.transition_count),
            Cell::new(&saved_label),
            Cell::new(format_duration(b.duration_ms as f64 / 1000.0)),
        ]);
        table.add_row(row);
    }
    println!("{table}");

    // Grand total
    let total_label = if report.total_bytes_saved >= 0 {
        format!("{} saved", format_size(report.total_bytes_saved as u64))
    } else {
        format!(
            "{} added",
            format_size(report.total_bytes_saved.unsigned_abs())
        )
    };
    println!(
        "\n  Total: {} across {} transitions",
        style(total_label).bold(),
        style(report.total_transitions).bold(),
    );
}

fn print_savings_plain(
    report: &voom_domain::stats::SavingsReport,
    show_period: bool,
) {
    for b in &report.buckets {
        let executor = b.executor.as_deref().unwrap_or("-");
        let phase = b.phase.as_deref().unwrap_or("-");
        if show_period {
            let period = b.period.as_deref().unwrap_or("-");
            println!(
                "{executor}\t{phase}\t{period}\t{}\t{}\t{}",
                b.file_count, b.transition_count, b.bytes_saved,
            );
        } else {
            println!(
                "{executor}\t{phase}\t{}\t{}\t{}",
                b.file_count, b.transition_count, b.bytes_saved,
            );
        }
    }
}
```

- [ ] **Step 4: Verify compilation**

Run: `cargo build`
Expected: Compiles without errors.

- [ ] **Step 5: Run all report tests**

Run: `cargo test -p voom-cli report`
Expected: All existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-cli/src/cli.rs crates/voom-cli/src/commands/report.rs
git commit -m "feat(cli): add voom report --savings flag with executor/phase/period breakdown"
```

---

### Task 5: Add a functional test for the savings report

**Files:**
- Modify: whichever file contains the `voom report` functional tests (find with `cargo test -p voom-cli --features functional -- report`)

- [ ] **Step 1: Find the functional test file for report**

Run: `grep -rl "report.*--stats\|fn.*report" crates/voom-cli/tests/`

This will identify where to add the test.

- [ ] **Step 2: Add a functional test**

Add to the appropriate functional test file. The test should:
1. Scan a test corpus
2. Process it (to create voom transitions)
3. Run `voom report --savings` and verify it produces output
4. Run `voom report --savings --period month -f json` and verify JSON schema

```rust
#[test]
fn report_savings_shows_executor_breakdown() {
    let env = TestEnv::new();
    env.scan();
    env.process();

    let output = env.run(&["report", "--savings"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Space Savings by Provenance")
            || stdout.contains("No savings data"),
        "expected savings header or empty message, got: {stdout}",
    );
}

#[test]
fn report_savings_json_format() {
    let env = TestEnv::new();
    env.scan();
    env.process();

    let output = env.run(&["report", "--savings", "-f", "json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect("savings JSON should be valid");
    assert!(parsed.get("buckets").is_some());
    assert!(parsed.get("total_bytes_saved").is_some());
    assert!(parsed.get("total_transitions").is_some());
}

#[test]
fn report_savings_with_period_flag() {
    let env = TestEnv::new();
    let output = env.run(&["report", "--savings", "--period", "month", "-f", "json"]);
    assert!(output.status.success());
}

#[test]
fn report_savings_invalid_period_fails() {
    let env = TestEnv::new();
    let output = env.run(&["report", "--savings", "--period", "century"]);
    assert!(!output.status.success());
}
```

Note: Adapt the test helper patterns (`TestEnv`, `env.run`, etc.) to match whatever pattern the existing functional tests use. Read the file first before writing.

- [ ] **Step 3: Run the functional tests**

Run: `cargo test -p voom-cli --features functional -- report_savings --test-threads=1`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/tests/
git commit -m "test(cli): add functional tests for voom report --savings"
```

---

### Task 6: Run full test suite and clippy

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --workspace`
Expected: No warnings.

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 3: Run functional tests**

Run: `cargo test -p voom-cli --features functional -- --test-threads=4`
Expected: All pass.

- [ ] **Step 4: Fix any issues found, then commit fixes if needed**
