use std::io::Write;

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use voom_domain::storage::EventLogFilters;

use crate::app;
use crate::cli::{EventsArgs, OutputFormat};
use crate::config;

/// Storage clamps `list_event_log` to 10_000 rows per call; we use the same
/// chunk size so a single call returns either everything matching or a full
/// page that we can advance past via `since_rowid`.
const EVENT_LOG_PAGE_SIZE: u32 = 10_000;

/// Page through the event log via `since_rowid`, invoking `f` on each record
/// in order, until either `max_total` records have been emitted or storage
/// returns fewer than `EVENT_LOG_PAGE_SIZE` rows (i.e., the table is drained).
fn fetch_paginated(
    store: &std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
    base_filters: &EventLogFilters,
    max_total: u32,
    mut f: impl FnMut(&voom_domain::storage::EventLogRecord) -> Result<()>,
) -> Result<()> {
    let mut emitted: u32 = 0;
    let mut since_rowid: i64 = base_filters.since_rowid.unwrap_or(0);

    while emitted < max_total {
        let mut page_filters = base_filters.clone();
        page_filters.since_rowid = Some(since_rowid);
        let remaining = max_total - emitted;
        let page_size = remaining.min(EVENT_LOG_PAGE_SIZE);
        page_filters.limit = Some(page_size);

        let page = store.list_event_log(&page_filters)?;
        let page_len = page.len() as u32;

        for record in &page {
            f(record)?;
            since_rowid = since_rowid.max(record.rowid);
            emitted += 1;
            if emitted >= max_total {
                break;
            }
        }

        // Storage returned fewer rows than the page size — we've drained the table.
        if page_len < page_size {
            break;
        }
    }

    Ok(())
}

/// Treat a broken pipe (e.g., `voom events | head`) as a clean exit instead of
/// propagating an error. Other I/O failures still bubble up.
fn ignore_broken_pipe(result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            for cause in err.chain() {
                if let Some(io_err) = cause.downcast_ref::<std::io::Error>() {
                    if io_err.kind() == std::io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                }
            }
            Err(err)
        }
    }
}

pub async fn run(args: EventsArgs, token: CancellationToken) -> Result<()> {
    let config = config::load_config().unwrap_or_default();
    let store = app::open_store(&config)?;

    let mut base_filters = EventLogFilters::default();
    base_filters.event_type = args.filter.clone();

    if args.follow {
        run_follow(store, &args, base_filters, token).await
    } else {
        run_streaming(&store, &base_filters, args.limit, args.format)
    }
}

fn run_streaming(
    store: &std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
    base_filters: &EventLogFilters,
    limit: u32,
    format: OutputFormat,
) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    match format {
        OutputFormat::Table => {
            let mut header_written = false;
            let mut any = false;
            ignore_broken_pipe(fetch_paginated(store, base_filters, limit, |r| {
                if !header_written {
                    writeln!(out, "{:<20} {:<28} SUMMARY", "TIMESTAMP", "TYPE")?;
                    writeln!(out, "{}", "-".repeat(78))?;
                    header_written = true;
                }
                any = true;
                writeln!(
                    out,
                    "{:<20} {:<28} {}",
                    r.created_at.format("%Y-%m-%d %H:%M:%S"),
                    r.event_type,
                    r.summary,
                )?;
                Ok(())
            }))?;
            if !any {
                eprintln!("No events found.");
            }
        }
        OutputFormat::Json => {
            ignore_broken_pipe(write_json_stream(&mut out, store, base_filters, limit))?;
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            ignore_broken_pipe(fetch_paginated(store, base_filters, limit, |r| {
                writeln!(
                    out,
                    "{}\t{}\t{}",
                    r.event_type,
                    r.created_at.format("%Y-%m-%d %H:%M:%S"),
                    r.summary,
                )?;
                Ok(())
            }))?;
        }
    }
    Ok(())
}

/// Stream the event log as a single JSON array, hand-formatted so output is
/// byte-compatible with the previous `serde_json::to_string_pretty(&Vec<_>)`
/// call site — downstream consumers (e.g., `voom events -f json | jq`) keep
/// working unchanged.
fn write_json_stream<W: Write>(
    out: &mut W,
    store: &std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
    base_filters: &EventLogFilters,
    limit: u32,
) -> Result<()> {
    let mut first = true;
    writeln!(out, "[")?;
    fetch_paginated(store, base_filters, limit, |r| {
        if !first {
            writeln!(out, ",")?;
        }
        first = false;
        let value = record_to_json(r);
        let s = serde_json::to_string_pretty(&value)?;
        for (i, line) in s.lines().enumerate() {
            if i == 0 {
                write!(out, "  {line}")?;
            } else {
                write!(out, "\n  {line}")?;
            }
        }
        Ok(())
    })?;
    writeln!(out)?;
    writeln!(out, "]")?;
    Ok(())
}

async fn run_follow(
    store: std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
    args: &EventsArgs,
    base_filters: EventLogFilters,
    token: CancellationToken,
) -> Result<()> {
    let mut last_rowid = 0i64;

    // Initial backfill — same behavior as non-follow mode, including pagination.
    fetch_paginated(&store, &base_filters, args.limit, |r| {
        print_follow_row(args.format, r);
        last_rowid = last_rowid.max(r.rowid);
        Ok(())
    })?;

    let mut poll_filters = base_filters.clone();
    poll_filters.limit = Some(200);

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! {
            () = token.cancelled() => break,
            _ = interval.tick() => {}
        }

        poll_filters.since_rowid = Some(last_rowid);
        let filters = poll_filters.clone();
        let store = store.clone();
        let new_records =
            tokio::task::spawn_blocking(move || store.list_event_log(&filters)).await??;

        for r in &new_records {
            print_follow_row(args.format, r);
            last_rowid = last_rowid.max(r.rowid);
        }
    }

    Ok(())
}

fn record_to_json(r: &voom_domain::storage::EventLogRecord) -> serde_json::Value {
    serde_json::json!({
        "rowid": r.rowid,
        "id": r.id.to_string(),
        "event_type": r.event_type,
        "summary": r.summary,
        "payload": serde_json::from_str::<serde_json::Value>(&r.payload)
            .unwrap_or_else(|_| serde_json::Value::String(r.payload.clone())),
        "created_at": r.created_at.to_rfc3339(),
    })
}

fn print_follow_row(format: OutputFormat, r: &voom_domain::storage::EventLogRecord) {
    match format {
        OutputFormat::Table => {
            println!(
                "{:<20} {:<28} {}",
                r.created_at.format("%Y-%m-%d %H:%M:%S"),
                r.event_type,
                r.summary,
            );
        }
        OutputFormat::Json => {
            println!("{}", record_to_json(r));
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            println!(
                "{}\t{}\t{}",
                r.event_type,
                r.created_at.format("%Y-%m-%d %H:%M:%S"),
                r.summary,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use voom_domain::storage::{EventLogRecord, EventLogStorage};
    use voom_domain::test_support::InMemoryStore;

    fn store_with_n_events(n: usize) -> Arc<dyn voom_domain::storage::StorageTrait> {
        let store = InMemoryStore::new();
        for i in 0..n {
            let record = EventLogRecord::new(
                uuid::Uuid::new_v4(),
                "file.discovered".into(),
                format!(r#"{{"n":{i}}}"#),
                format!("event {i}"),
            );
            store.insert_event_log(&record).expect("insert");
        }
        Arc::new(store)
    }

    #[test]
    fn fetch_paginated_returns_all_when_under_page_size() {
        let store = store_with_n_events(50);
        let filters = EventLogFilters::default();
        let mut total = 0;
        fetch_paginated(&store, &filters, 1000, |r| {
            assert_eq!(r.event_type, "file.discovered");
            total += 1;
            Ok(())
        })
        .expect("paginate");
        assert_eq!(total, 50);
    }

    #[test]
    fn fetch_paginated_returns_more_than_page_size() {
        let store = store_with_n_events(25_000);
        let filters = EventLogFilters::default();
        let mut total = 0;
        fetch_paginated(&store, &filters, 25_000, |_| {
            total += 1;
            Ok(())
        })
        .expect("paginate");
        assert_eq!(total, 25_000);
    }

    #[test]
    fn fetch_paginated_respects_max_total() {
        let store = store_with_n_events(25_000);
        let filters = EventLogFilters::default();
        let mut total = 0;
        fetch_paginated(&store, &filters, 12_345, |_| {
            total += 1;
            Ok(())
        })
        .expect("paginate");
        assert_eq!(total, 12_345);
    }
}
