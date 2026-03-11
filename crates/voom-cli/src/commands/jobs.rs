use anyhow::Result;
use owo_colors::OwoColorize;

use crate::cli::JobsCommands;

pub async fn run(cmd: JobsCommands) -> Result<()> {
    match cmd {
        JobsCommands::List { status } => list(status).await,
        JobsCommands::Status { id } => status(id).await,
        JobsCommands::Cancel { id } => cancel(id).await,
    }
}

async fn list(status_filter: Option<String>) -> Result<()> {
    // The StorageTrait doesn't have a list_jobs method yet.
    println!(
        "{} Job listing requires storage trait extension (list_jobs).",
        "NOTE".bold().yellow()
    );
    if let Some(ref s) = status_filter {
        println!("Filter: status={s}");
    }
    println!("{} No jobs found.", "INFO".dimmed());

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
            println!("{} Job {id} not found.", "ERROR".bold().red());
            std::process::exit(1);
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
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("{} Job {id} cancelled.", "OK".bold().green());

    Ok(())
}
