use anyhow::Result;

use crate::cli::BugReportGenerateArgs;
use crate::commands::bug_report::redactor::{RedactionKind, Redactor};

#[derive(Debug)]
pub struct BugReportBundle {
    pub out_dir: std::path::PathBuf,
}

pub fn collect(args: BugReportGenerateArgs) -> Result<BugReportBundle> {
    let mut redactor = Redactor::default();
    let _ = redactor.redact_text(&args.out.display().to_string());
    let _ = redactor.redact_json(serde_json::Value::Null);
    let _ = redactor.private_mappings();
    let _ = redactor.report();
    let _ = RedactionKind::PathComponent;
    Ok(BugReportBundle { out_dir: args.out })
}
