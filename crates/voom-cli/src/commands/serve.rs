use anyhow::Result;
use owo_colors::OwoColorize;

use crate::cli::ServeArgs;

pub async fn run(args: ServeArgs) -> Result<()> {
    println!(
        "{} Web server is not yet implemented.",
        "TODO".bold().yellow()
    );
    println!(
        "Would listen on {}:{}",
        args.host.cyan(),
        args.port.to_string().cyan()
    );
    println!("This will be implemented in Sprint 10 (Web UI).");

    Ok(())
}
