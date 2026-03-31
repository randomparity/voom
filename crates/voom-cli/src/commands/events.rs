use anyhow::Result;
use tokio_util::sync::CancellationToken;
use voom_domain::storage::EventLogFilters;

use crate::app;
use crate::cli::{EventsArgs, OutputFormat};
use crate::config;

pub async fn run(args: EventsArgs, token: CancellationToken) -> Result<()> {
    let config = config::load_config().unwrap_or_default();
    let store = app::open_store(&config)?;

    let mut filters = EventLogFilters::default();
    filters.event_type = args.filter.clone();
    filters.limit = Some(args.limit);

    let records = store.list_event_log(&filters)?;

    if args.follow {
        run_follow(store, args, records, token).await
    } else {
        run_default(args.format, records)
    }
}

fn run_default(
    format: OutputFormat,
    records: Vec<voom_domain::storage::EventLogRecord>,
) -> Result<()> {
    match format {
        OutputFormat::Table => {
            if records.is_empty() {
                println!("No events found.");
                return Ok(());
            }
            println!("{:<20} {:<28} SUMMARY", "TIMESTAMP", "TYPE");
            println!("{}", "-".repeat(78));
            for r in &records {
                println!(
                    "{:<20} {:<28} {}",
                    r.created_at.format("%Y-%m-%d %H:%M:%S"),
                    r.event_type,
                    r.summary,
                );
            }
        }
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = records.iter().map(record_to_json).collect();
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        OutputFormat::Plain => {
            for r in &records {
                println!(
                    "{}\t{}\t{}",
                    r.event_type,
                    r.created_at.format("%Y-%m-%d %H:%M:%S"),
                    r.summary,
                );
            }
        }
    }
    Ok(())
}

async fn run_follow(
    store: std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
    args: EventsArgs,
    initial: Vec<voom_domain::storage::EventLogRecord>,
    token: CancellationToken,
) -> Result<()> {
    let mut last_rowid = 0i64;

    // Print initial batch
    for r in &initial {
        print_follow_row(&args.format, r);
        last_rowid = last_rowid.max(r.rowid);
    }

    let mut poll_filters = EventLogFilters::default();
    poll_filters.event_type = args.filter.clone();
    poll_filters.limit = Some(200);

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = interval.tick() => {}
        }

        poll_filters.since_rowid = Some(last_rowid);
        let filters = poll_filters.clone();
        let store = store.clone();
        let new_records =
            tokio::task::spawn_blocking(move || store.list_event_log(&filters)).await??;

        for r in &new_records {
            print_follow_row(&args.format, r);
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

fn print_follow_row(format: &OutputFormat, r: &voom_domain::storage::EventLogRecord) {
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
        OutputFormat::Plain => {
            println!(
                "{}\t{}\t{}",
                r.event_type,
                r.created_at.format("%Y-%m-%d %H:%M:%S"),
                r.summary,
            );
        }
    }
}
