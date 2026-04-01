use std::path::Path;

use anyhow::{Context, Result};
use comfy_table::{Cell, Color};
use console::style;
use uuid::Uuid;

use voom_domain::storage::{FileStorage, PlanStatus};

use crate::cli::{OutputFormat, PlansCommands};
use crate::{app, config, output};

pub fn run(cmd: PlansCommands) -> Result<()> {
    match cmd {
        PlansCommands::Show { file, format } => show(&file, format),
    }
}

fn resolve_file_id(file_arg: &str, store: &dyn FileStorage) -> Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(file_arg) {
        return Ok(uuid);
    }

    let canonical =
        std::fs::canonicalize(file_arg).unwrap_or_else(|_| Path::new(file_arg).to_path_buf());
    let media_file = store
        .file_by_path(&canonical)
        .context("failed to look up file by path")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No file found for '{file_arg}'. \
                 Provide a valid UUID or a path to a scanned file."
            )
        })?;
    Ok(media_file.id)
}

fn show(file_arg: &str, format: OutputFormat) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let file_id = resolve_file_id(file_arg, store.as_ref())?;
    let plans = store
        .plans_for_file(&file_id)
        .context("failed to load plans")?;

    if plans.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!("{}", style("No plans found for this file.").yellow());
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&plans)
                    .expect("PlanSummary serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            let mut table = output::new_table();
            table.set_header(vec![
                "Phase",
                "Status",
                "Actions",
                "Skip Reason",
                "Evaluated",
                "Executed",
            ]);

            for plan in &plans {
                let status_cell = styled_status_cell(plan.status);
                let action_count = plan.actions.len().to_string();
                let skip = plan.skip_reason.as_deref().unwrap_or("-");
                let evaluated = plan
                    .evaluated_at
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "-".into());
                let executed = plan
                    .executed_at
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "-".into());

                table.add_row(vec![
                    Cell::new(&plan.phase_name),
                    status_cell,
                    Cell::new(&action_count),
                    Cell::new(skip),
                    Cell::new(&evaluated),
                    Cell::new(&executed),
                ]);
            }

            println!("{table}");
        }
        // Plans are complex structures; fall through to JSON for plain output
        OutputFormat::Plain => {
            println!(
                "{}",
                serde_json::to_string_pretty(&plans)
                    .expect("PlanSummary serialization cannot fail")
            );
        }
    }

    Ok(())
}

fn styled_status_cell(status: PlanStatus) -> Cell {
    let color = match status {
        PlanStatus::Pending => Some(Color::Yellow),
        PlanStatus::Executing => Some(Color::Cyan),
        PlanStatus::Completed => Some(Color::Green),
        PlanStatus::Failed => Some(Color::Red),
        PlanStatus::Skipped => Some(Color::DarkGrey),
    };
    let mut cell = Cell::new(status.as_str());
    if let Some(c) = color {
        cell = cell.fg(c);
    }
    cell
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::storage::PlanStatus;

    #[test]
    fn test_styled_status_cell_all_variants() {
        for status in [
            PlanStatus::Pending,
            PlanStatus::Executing,
            PlanStatus::Completed,
            PlanStatus::Failed,
            PlanStatus::Skipped,
        ] {
            let cell = styled_status_cell(status);
            let content = cell.content().to_string();
            assert_eq!(content, status.as_str());
        }
    }
}
