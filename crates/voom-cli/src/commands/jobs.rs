use anyhow::Result;
use comfy_table::Cell;
use owo_colors::OwoColorize;

use crate::cli::JobsCommands;
use crate::output;

pub async fn run(cmd: JobsCommands) -> Result<()> {
    match cmd {
        JobsCommands::List { status } => list(status).await,
        JobsCommands::Status { id } => status(id).await,
        JobsCommands::Cancel { id } => cancel(id).await,
    }
}

async fn list(status_filter: Option<String>) -> Result<()> {
    let config = crate::app::load_config()?;
    let store = crate::app::open_store(&config)?;

    use voom_domain::job::JobStatus;
    use voom_domain::storage::StorageTrait;

    let filter_status = status_filter.as_deref().and_then(JobStatus::parse);

    let jobs = store
        .list_jobs(filter_status, Some(50))
        .map_err(|e| anyhow::anyhow!("failed to list jobs: {e}"))?;

    if jobs.is_empty() {
        println!("{} No jobs found.", "INFO".dimmed());
        return Ok(());
    }

    let mut table = output::new_table();
    table.set_header(vec![
        "ID", "Type", "Status", "Progress", "Worker", "Created",
    ]);

    for job in &jobs {
        let status_cell = match job.status {
            JobStatus::Pending => job.status.as_str().yellow().to_string(),
            JobStatus::Running => job.status.as_str().cyan().to_string(),
            JobStatus::Completed => job.status.as_str().green().to_string(),
            JobStatus::Failed => job.status.as_str().red().to_string(),
            JobStatus::Cancelled => job.status.as_str().dimmed().to_string(),
            _ => job.status.as_str().to_string(),
        };

        table.add_row(vec![
            Cell::new(&job.id.to_string()[..8]),
            Cell::new(&job.job_type),
            Cell::new(status_cell),
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
        let summary: Vec<String> = counts
            .iter()
            .map(|(status, count)| format!("{}: {count}", status.as_str()))
            .collect();
        println!("\n{}", summary.join(" | ").dimmed());
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
