use anyhow::Result;

use crate::commands::bug_report::collect::BugReportBundle;

pub fn write_bundle(bundle: &BugReportBundle) -> Result<()> {
    let _ = &bundle.out_dir;
    Ok(())
}
