use anyhow::Result;
use comfy_table::{Cell, Color};
use owo_colors::OwoColorize;

use crate::cli::JobsCommands;
use crate::output;

pub async fn run(cmd: JobsCommands) -> Result<()> {
    match cmd {
        JobsCommands::List { status, limit } => list(status, limit).await,
        JobsCommands::Status { id } => status(id).await,
        JobsCommands::Cancel { id } => cancel(id).await,
    }
}

async fn list(status_filter: Option<String>, limit: u32) -> Result<()> {
    let config = crate::app::load_config()?;
    let store = crate::app::open_store(&config)?;

    use voom_domain::job::JobStatus;
    use voom_domain::storage::StorageTrait;

    let filter_status = status_filter.as_deref().and_then(JobStatus::parse);

    let jobs = store
        .list_jobs(filter_status, Some(limit))
        .map_err(|e| anyhow::anyhow!("failed to list jobs: {e}"))?;

    if jobs.is_empty() {
        println!("{} No jobs found.", "INFO".dimmed());
        return Ok(());
    }

    let mut table = output::new_table();
    table.set_header(vec![
        "ID", "Type", "File", "Status", "Progress", "Worker", "Created",
    ]);

    for job in &jobs {
        let status_color = match job.status {
            JobStatus::Pending => Some(Color::Yellow),
            JobStatus::Running => Some(Color::Cyan),
            JobStatus::Completed => Some(Color::Green),
            JobStatus::Failed => Some(Color::Red),
            JobStatus::Cancelled => Some(Color::DarkGrey),
            _ => None,
        };
        let mut status_cell = Cell::new(job.status.as_str());
        if let Some(color) = status_color {
            status_cell = status_cell.fg(color);
        }

        let file_name = job
            .payload
            .as_ref()
            .and_then(|p| p["path"].as_str())
            .and_then(|p| std::path::Path::new(p).file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        table.add_row(vec![
            Cell::new(&job.id.to_string()[..8]),
            Cell::new(&job.job_type),
            Cell::new(&file_name),
            status_cell,
            Cell::new(format!("{:.0}%", job.progress * 100.0)),
            Cell::new(job.worker_id.as_deref().unwrap_or("-")),
            Cell::new(job.created_at.format("%Y-%m-%d %H:%M")),
        ]);
    }

    println!("{table}");

    // Show summary counts
    let counts = store
        .count_jobs_by_status()
        .map_err(|e| anyhow::anyhow!("failed to count jobs by status: {e}"))?;
    if !counts.is_empty() {
        let total: u64 = counts.iter().map(|(_, c)| c).sum();
        let summary: Vec<String> = counts
            .iter()
            .map(|(status, count)| format!("{}: {count}", status.as_str()))
            .collect();
        let shown = jobs.len() as u64;
        if shown < total {
            println!(
                "\n{} {}",
                format!("Showing {shown} of {total} jobs.").dimmed(),
                "Use --limit or --status to narrow results.".dimmed(),
            );
        }
        println!("{}", summary.join(" | ").dimmed());
    }

    Ok(())
}

async fn status(id: String) -> Result<()> {
    let config = crate::app::load_config()?;
    let store = crate::app::open_store(&config)?;

    let uuid = uuid::Uuid::parse_str(&id).map_err(|_| anyhow::anyhow!("Invalid job ID: {id}"))?;

    use voom_domain::storage::StorageTrait;
    match store.get_job(&uuid)? {
        Some(job) => {
            println!("{} {}", "Job:".bold(), job.id.to_string().cyan());
            println!("{} {}", "Type:".bold(), job.job_type);
            if let Some(ref payload) = job.payload {
                if let Some(path) = payload["path"].as_str() {
                    println!("{} {}", "File:".bold(), path);
                }
            }
            println!("{} {}", "Status:".bold(), job.status.as_str());
            println!("{} {:.1}%", "Progress:".bold(), job.progress * 100.0);
            if let Some(ref msg) = job.progress_message {
                println!("{} {msg}", "Message:".bold());
            }
            if let Some(ref err) = job.error {
                println!("{} {err}", "Error:".bold().red());
            }
            println!("{} {}", "Created:".bold(), job.created_at);
            if let Some(ref started) = job.started_at {
                println!("{} {started}", "Started:".bold());
            }
            if let Some(ref completed) = job.completed_at {
                println!("{} {completed}", "Completed:".bold());
            }
        }
        None => {
            anyhow::bail!("Job {id} not found");
        }
    }

    Ok(())
}

async fn cancel(id: String) -> Result<()> {
    let config = crate::app::load_config()?;
    let store = crate::app::open_store(&config)?;

    let uuid = uuid::Uuid::parse_str(&id).map_err(|_| anyhow::anyhow!("Invalid job ID: {id}"))?;

    use voom_domain::storage::StorageTrait;
    let update = voom_domain::JobUpdate {
        status: Some(voom_domain::JobStatus::Cancelled),
        ..Default::default()
    };

    store
        .update_job(&uuid, &update)
        .map_err(|e| anyhow::anyhow!("failed to cancel job: {e}"))?;

    println!("{} Job {id} cancelled.", "OK".bold().green());

    Ok(())
}

#[cfg(test)]
mod tests {
    use voom_domain::job::JobStatus;

    #[test]
    fn job_status_parse_valid_values() {
        assert_eq!(JobStatus::parse("pending"), Some(JobStatus::Pending));
        assert_eq!(JobStatus::parse("running"), Some(JobStatus::Running));
        assert_eq!(JobStatus::parse("completed"), Some(JobStatus::Completed));
        assert_eq!(JobStatus::parse("failed"), Some(JobStatus::Failed));
        assert_eq!(JobStatus::parse("cancelled"), Some(JobStatus::Cancelled));
    }

    #[test]
    fn job_status_parse_invalid_returns_none() {
        assert_eq!(JobStatus::parse("unknown"), None);
        assert_eq!(JobStatus::parse(""), None);
    }

    #[test]
    fn job_status_as_str_roundtrip() {
        let statuses = [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ];
        for status in &statuses {
            let s = status.as_str();
            assert_eq!(JobStatus::parse(s), Some(*status));
        }
    }

    #[test]
    fn uuid_parse_valid() {
        let valid = "550e8400-e29b-41d4-a716-446655440000";
        assert!(uuid::Uuid::parse_str(valid).is_ok());
    }

    #[test]
    fn uuid_parse_invalid() {
        assert!(uuid::Uuid::parse_str("not-a-uuid").is_err());
    }
}
