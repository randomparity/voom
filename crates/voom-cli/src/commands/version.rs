use anyhow::Result;
use serde::Serialize;

use crate::cli::OutputFormat;
use crate::output;

#[derive(Serialize)]
struct VersionMetadata {
    command: &'static str,
    product_version: &'static str,
    package_version: &'static str,
    git_sha: &'static str,
    git_dirty: bool,
    build_profile: &'static str,
    target: &'static str,
}

pub fn run(format: OutputFormat) -> Result<()> {
    let metadata = VersionMetadata {
        command: "version",
        product_version: env!("VOOM_PRODUCT_VERSION"),
        package_version: env!("CARGO_PKG_VERSION"),
        git_sha: env!("VOOM_GIT_SHA"),
        git_dirty: env!("VOOM_GIT_DIRTY") == "true",
        build_profile: build_profile(),
        target: env!("VOOM_BUILD_TARGET"),
    };

    match format {
        OutputFormat::Json => output::print_json(&metadata),
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Csv => {
            println!("voom {}", metadata.product_version);
            Ok(())
        }
    }
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
