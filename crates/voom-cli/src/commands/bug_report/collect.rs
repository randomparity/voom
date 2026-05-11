use anyhow::Result;

use crate::cli::BugReportGenerateArgs;

#[derive(Debug)]
pub struct BugReportBundle {
    pub out_dir: std::path::PathBuf,
}

pub fn collect(args: BugReportGenerateArgs) -> Result<BugReportBundle> {
    Ok(BugReportBundle { out_dir: args.out })
}
