//! Tool detector plugin: discovers and caches external tool availability and versions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::ToolDetectedEvent;
use voom_kernel::{Plugin, PluginContext};

/// A detected external tool with its path and version.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DetectedTool {
    pub name: String,
    pub version: String,
    pub path: PathBuf,
}

/// Known tools that the detector can find.
const KNOWN_TOOLS: &[(&str, &[&str])] = &[
    ("ffprobe", &["-version"]),
    ("ffmpeg", &["-version"]),
    ("mkvmerge", &["--version"]),
    ("mkvpropedit", &["--version"]),
    ("mkvextract", &["--version"]),
    ("mediainfo", &["--version"]),
    ("HandBrakeCLI", &["--version"]),
];

/// Tool detector plugin: finds external tools (ffprobe, ffmpeg, mkvtoolnix) on PATH.
///
/// Caches detection results for the lifetime of the plugin. Use `detect_all()`
/// to refresh the cache.
pub struct ToolDetectorPlugin {
    capabilities: Vec<Capability>,
    cache: HashMap<String, DetectedTool>,
}

impl ToolDetectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::DetectTools],
            cache: HashMap::new(),
        }
    }

    /// Populate the tool cache by detecting all known tools on PATH.
    fn populate_cache(&mut self) {
        self.cache.clear();
        for &(name, args) in KNOWN_TOOLS {
            if let Some(tool) = detect_tool(name, args) {
                tracing::info!(
                    tool = %tool.name,
                    version = %tool.version,
                    path = %tool.path.display(),
                    "tool detected"
                );
                self.cache.insert(tool.name.clone(), tool);
            } else {
                tracing::debug!(tool = name, "tool not found");
            }
        }
    }

    /// Detect all known tools, refresh the cache, and return events for each found tool.
    pub fn detect_all(&mut self) -> Vec<ToolDetectedEvent> {
        self.populate_cache();
        self.cache
            .values()
            .map(|tool| {
                ToolDetectedEvent::new(tool.name.clone(), tool.version.clone(), tool.path.clone())
            })
            .collect()
    }

    /// Check if a specific tool is available.
    #[must_use]
    pub fn is_available(&self, name: &str) -> bool {
        self.cache.contains_key(name)
    }

    /// Get a detected tool by name.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&DetectedTool> {
        self.cache.get(name)
    }

    /// Get the path to a tool, returning an error if not found.
    pub fn require_tool(&self, name: &str) -> Result<&DetectedTool> {
        self.cache.get(name).ok_or_else(|| VoomError::ToolNotFound {
            tool: name.to_string(),
        })
    }

    /// Get all detected tools.
    #[must_use]
    pub fn detected_tools(&self) -> &HashMap<String, DetectedTool> {
        &self.cache
    }
}

impl Default for ToolDetectorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ToolDetectorPlugin {
    fn name(&self) -> &str {
        "tool-detector"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<voom_domain::events::Event>> {
        let events = self.detect_all();
        tracing::info!(
            found = events.len(),
            total = KNOWN_TOOLS.len(),
            "tool detection complete"
        );
        Ok(events
            .into_iter()
            .map(voom_domain::events::Event::ToolDetected)
            .collect())
    }
}

/// Detect a tool by running it and parsing the version output.
fn detect_tool(name: &str, args: &[&str]) -> Option<DetectedTool> {
    let output = Command::new(name).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = parse_version(name, &stdout);
    let path = find_tool_path(name).unwrap_or_else(|| PathBuf::from(name));

    Some(DetectedTool {
        name: name.to_string(),
        version,
        path,
    })
}

/// Parse version string from tool output.
fn parse_version(tool_name: &str, output: &str) -> String {
    let first_line = output.lines().next().unwrap_or("");

    match tool_name {
        "ffprobe" | "ffmpeg" => {
            // "ffprobe version 6.1.1 Copyright ..."
            first_line
                .split_whitespace()
                .nth(2)
                .unwrap_or("unknown")
                .to_string()
        }
        "mkvmerge" | "mkvpropedit" | "mkvextract" => {
            // "mkvmerge v82.0 ('I'm The President') 64-bit"
            first_line
                .split_whitespace()
                .nth(1)
                .map(|v| v.trim_start_matches('v'))
                .unwrap_or("unknown")
                .to_string()
        }
        "mediainfo" => {
            // "MediaInfo Command line, MediaInfoLib - v24.01"
            first_line
                .split('v')
                .next_back()
                .unwrap_or("unknown")
                .trim()
                .to_string()
        }
        "HandBrakeCLI" => {
            // "HandBrake 1.8.2" or "HandBrake 20240621000000-e9ff2bd-unknown"
            first_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("unknown")
                .to_string()
        }
        _ => first_line.to_string(),
    }
}

/// Find the full path to a tool using `which`.
fn find_tool_path(name: &str) -> Option<PathBuf> {
    Command::new("which").arg(name).output().ok().and_then(|o| {
        if o.status.success() {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !path.is_empty() {
                Some(PathBuf::from(path))
            } else {
                None
            }
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::Event;

    #[test]
    fn test_plugin_metadata() {
        let plugin = ToolDetectorPlugin::new();
        assert_eq!(plugin.name(), "tool-detector");
        assert_eq!(plugin.capabilities()[0].kind(), "detect_tools");
    }

    #[test]
    fn test_handles_no_events() {
        let plugin = ToolDetectorPlugin::new();
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::TOOL_DETECTED));
    }

    #[test]
    fn test_require_tool_not_found() {
        let plugin = ToolDetectorPlugin::new();
        let result = plugin.require_tool("nonexistent-tool");
        assert!(result.is_err());
        match result.unwrap_err() {
            VoomError::ToolNotFound { tool } => {
                assert_eq!(tool, "nonexistent-tool");
            }
            other => panic!("expected ToolNotFound, got: {other}"),
        }
    }

    #[test]
    fn test_is_available_empty_cache() {
        let plugin = ToolDetectorPlugin::new();
        assert!(!plugin.is_available("ffprobe"));
        assert!(!plugin.is_available("ffmpeg"));
    }

    #[test]
    fn test_parse_version_ffprobe() {
        let output = "ffprobe version 6.1.1 Copyright (c) 2007-2023 the FFmpeg developers";
        assert_eq!(parse_version("ffprobe", output), "6.1.1");
    }

    #[test]
    fn test_parse_version_ffmpeg() {
        let output = "ffmpeg version 7.0.2 Copyright (c) 2000-2024 the FFmpeg developers";
        assert_eq!(parse_version("ffmpeg", output), "7.0.2");
    }

    #[test]
    fn test_parse_version_mkvmerge() {
        let output = "mkvmerge v82.0 ('I'm The President') 64-bit";
        assert_eq!(parse_version("mkvmerge", output), "82.0");
    }

    #[test]
    fn test_parse_version_handbrake() {
        assert_eq!(parse_version("HandBrakeCLI", "HandBrake 1.8.2"), "1.8.2");
    }

    #[test]
    fn test_parse_version_handbrake_dev() {
        assert_eq!(
            parse_version("HandBrakeCLI", "HandBrake 20240621000000-e9ff2bd-unknown"),
            "20240621000000-e9ff2bd-unknown"
        );
    }

    #[test]
    fn test_parse_version_empty() {
        assert_eq!(parse_version("ffprobe", ""), "unknown");
    }

    #[test]
    fn test_detect_all_populates_cache() {
        let mut plugin = ToolDetectorPlugin::new();
        let _events = plugin.detect_all();
        // We can't assert specific tools exist (depends on system),
        // but the cache should be populated for any found tools
        for (name, _) in KNOWN_TOOLS {
            if plugin.is_available(name) {
                let tool = plugin.tool(name).unwrap();
                assert_eq!(tool.name, *name);
                assert!(!tool.version.is_empty());
            }
        }
    }
}
