use anyhow::Result;

use crate::cli::BugReportCommands;

pub mod collect;
pub mod redactor;
pub mod render;
pub mod upload;

pub fn run(command: BugReportCommands) -> Result<()> {
    match command {
        BugReportCommands::Generate(args) => {
            let bundle = collect::collect(args)?;
            render::write_bundle(&bundle)
        }
        BugReportCommands::Upload(args) => upload::run(args),
    }
}
