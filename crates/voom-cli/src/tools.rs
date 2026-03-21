//! Tool detection helpers shared by `doctor` and `init` commands.

use console::style;

/// Result of checking required and optional external tools.
pub struct ToolCheckResult {
    /// Number of required tools that were not found.
    pub missing_required: u32,
}

/// Print the status of required and optional external tools using a
/// `ToolDetectorPlugin`. Returns a summary indicating how many required
/// tools are missing.
pub fn print_tool_status(detector: &voom_tool_detector::ToolDetectorPlugin) -> ToolCheckResult {
    let required_tools = ["ffprobe", "ffmpeg", "mkvmerge", "mkvpropedit"];
    let optional_tools = ["mkvextract", "mediainfo", "HandBrakeCLI"];

    let mut missing_required = 0u32;

    for tool in required_tools {
        print!("  {tool} ... ");
        if let Some(t) = detector.get_tool(tool) {
            println!("{} ({})", style("OK").green(), style(&t.version).dim());
        } else {
            println!("{} (required)", style("NOT FOUND").red());
            missing_required += 1;
        }
    }

    for tool in optional_tools {
        print!("  {tool} ... ");
        if let Some(t) = detector.get_tool(tool) {
            println!("{} ({})", style("OK").green(), style(&t.version).dim());
        } else {
            println!("{}", style("not found").yellow());
        }
    }

    ToolCheckResult { missing_required }
}
