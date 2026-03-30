use anyhow::{Context, Result};
use comfy_table::{Cell, Color};
use console::style;

use crate::cli::JobsCommands;
use crate::output;

pub fn run(cmd: JobsCommands) -> Result<()> {
    match cmd {
        JobsCommands::List {
            status,
            limit,
            offset,
        } => list(status, limit, offset),
        JobsCommands::Status { id } => status(id),
        JobsCommands::Cancel { id } => cancel(id),
        JobsCommands::Retry { id } => retry(id),
        JobsCommands::Clear { status, yes } => clear(status, yes),
    }
}

fn list(status_filter: Option<String>, limit: u32, offset: u32) -> Result<()> {
    let config = crate::config::load_config()?;
    let store = crate::app::open_store(&config)?;

    use voom_domain::job::JobStatus;
    use voom_domain::storage::JobFilters;

    let filter_status = match status_filter.as_deref() {
        Some(s) => {
            let parsed = JobStatus::parse(s);
            if parsed.is_none() {
                anyhow::bail!(
                    "Invalid job status '{s}'. Valid values: pending, running, completed, failed, cancelled"
                );
            }
            parsed
        }
        None => None,
    };

    let jobs = store
        .list_jobs(&{
            let mut f = JobFilters::default();
            f.status = filter_status;
            f.limit = Some(limit);
            if offset > 0 {
                f.offset = Some(offset);
            }
            f
        })
        .context("failed to list jobs")?;

    if jobs.is_empty() {
        println!("{} No jobs found.", style("INFO").dim());
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
            Cell::new(job.job_type.as_str()),
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
        .context("failed to count jobs by status")?;
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
                style(format!("Showing {shown} of {total} jobs.")).dim(),
                style("Use --limit, --offset, or --status to narrow results.").dim(),
            );
        }
        println!("{}", style(summary.join(" | ")).dim());
    }

    Ok(())
}

fn status(id: String) -> Result<()> {
    let config = crate::config::load_config()?;
    let store = crate::app::open_store(&config)?;

    let uuid = uuid::Uuid::parse_str(&id).with_context(|| format!("Invalid job ID: {id}"))?;

    match store.job(&uuid)? {
        Some(job) => {
            println!("{} {}", style("Job:").bold(), style(&job.id).cyan());
            println!("{} {}", style("Type:").bold(), job.job_type);
            if let Some(ref payload) = job.payload {
                if let Some(path) = payload["path"].as_str() {
                    println!("{} {}", style("File:").bold(), path);
                }
            }
            println!("{} {}", style("Status:").bold(), job.status.as_str());
            println!("{} {:.1}%", style("Progress:").bold(), job.progress * 100.0);
            if let Some(ref msg) = job.progress_message {
                println!("{} {msg}", style("Message:").bold());
            }
            if let Some(ref err) = job.error {
                println!("{} {err}", style("Error:").bold().red());
            }
            println!("{} {}", style("Created:").bold(), job.created_at);
            if let Some(ref started) = job.started_at {
                println!("{} {started}", style("Started:").bold());
            }
            if let Some(ref completed) = job.completed_at {
                println!("{} {completed}", style("Completed:").bold());
            }
        }
        None => {
            anyhow::bail!("Job {id} not found");
        }
    }

    Ok(())
}

fn cancel(id: String) -> Result<()> {
    let config = crate::config::load_config()?;
    let store = crate::app::open_store(&config)?;

    let uuid = uuid::Uuid::parse_str(&id).with_context(|| format!("Invalid job ID: {id}"))?;

    // Check that the job exists and is not already in a terminal state
    let job = store
        .job(&uuid)?
        .ok_or_else(|| anyhow::anyhow!("Job {id} not found"))?;

    if job.is_terminal() {
        anyhow::bail!(
            "Cannot cancel job {id}: already in terminal state '{}'",
            job.status.as_str()
        );
    }

    let mut update = voom_domain::JobUpdate::default();
    update.status = Some(voom_domain::JobStatus::Cancelled);
    update.completed_at = Some(Some(chrono::Utc::now()));

    store
        .update_job(&uuid, &update)
        .context("failed to cancel job")?;

    println!("{} Job {id} cancelled.", style("OK").bold().green());

    Ok(())
}

fn retry(id: String) -> Result<()> {
    let config = crate::config::load_config()?;
    let store = crate::app::open_store(&config)?;

    let uuid = uuid::Uuid::parse_str(&id).with_context(|| format!("Invalid job ID: {id}"))?;

    let job = store
        .job(&uuid)?
        .ok_or_else(|| anyhow::anyhow!("Job {id} not found"))?;

    if job.status != voom_domain::JobStatus::Failed {
        anyhow::bail!(
            "Cannot retry job {id}: status is '{}', only failed jobs can be retried",
            job.status.as_str()
        );
    }

    let mut new_job = voom_domain::Job::new(job.job_type.clone());
    new_job.priority = job.priority;
    new_job.payload.clone_from(&job.payload);

    let new_id = store
        .create_job(&new_job)
        .context("failed to create retry job")?;

    println!(
        "{} Created retry job {} from failed job {}",
        style("OK").bold().green(),
        style(&new_id.to_string()[..8]).cyan(),
        style(&id[..8.min(id.len())]).dim(),
    );

    Ok(())
}

fn clear(status_filter: Option<String>, yes: bool) -> Result<()> {
    use voom_domain::job::JobStatus;

    let filter_status = match status_filter.as_deref() {
        Some(s) => {
            let parsed = JobStatus::parse(s).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid job status '{s}'. \
                     Valid values: completed, failed, cancelled"
                )
            })?;
            if !matches!(
                parsed,
                JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
            ) {
                anyhow::bail!(
                    "Cannot clear '{s}' jobs: \
                     only terminal statuses (completed, failed, cancelled) \
                     can be cleared"
                );
            }
            Some(parsed)
        }
        None => None,
    };

    let config = crate::config::load_config()?;
    let store = crate::app::open_store(&config)?;

    let label = match filter_status {
        Some(s) => format!("all {} jobs", s.as_str()),
        None => "all completed, failed, and cancelled jobs".to_string(),
    };

    if !yes && !crate::output::confirm(&format!("Delete {label}?"))? {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    let deleted = store
        .delete_jobs(filter_status)
        .context("failed to delete jobs")?;

    println!(
        "{} Deleted {deleted} job{}.",
        style("OK").bold().green(),
        if deleted == 1 { "" } else { "s" },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use voom_domain::job::JobStatus;

    #[test]
    fn test_job_status_parse_valid_values() {
        assert_eq!(JobStatus::parse("pending"), Some(JobStatus::Pending));
        assert_eq!(JobStatus::parse("running"), Some(JobStatus::Running));
        assert_eq!(JobStatus::parse("completed"), Some(JobStatus::Completed));
        assert_eq!(JobStatus::parse("failed"), Some(JobStatus::Failed));
        assert_eq!(JobStatus::parse("cancelled"), Some(JobStatus::Cancelled));
    }

    #[test]
    fn test_job_status_parse_invalid_returns_none() {
        assert_eq!(JobStatus::parse("unknown"), None);
        assert_eq!(JobStatus::parse(""), None);
    }

    #[test]
    fn test_job_status_as_str_roundtrip() {
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
    fn test_uuid_parse_valid() {
        let valid = "550e8400-e29b-41d4-a716-446655440000";
        assert!(uuid::Uuid::parse_str(valid).is_ok());
    }

    #[test]
    fn test_uuid_parse_invalid() {
        assert!(uuid::Uuid::parse_str("not-a-uuid").is_err());
    }
}
