//! Tool detector plugin: discovers and caches external tool availability and versions.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    ("nvidia-smi", &["--version"]),
    ("vainfo", &["--version"]),
    ("hdr10plus_tool", &["--version"]),
    ("dovi_tool", &["--version"]),
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
    fn name(&self) -> &'static str {
        "tool-detector"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

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
    let output = voom_process::run_with_timeout(name, args, Duration::from_secs(10)).ok()?;
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
                .map_or("unknown", |v| v.trim_start_matches('v'))
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
        "nvidia-smi" => {
            // "NVIDIA-SMI version  : 580.126.18"
            first_line
                .split(':')
                .next_back()
                .map_or("unknown", str::trim)
                .to_string()
        }
        "vainfo" => {
            // "vainfo: VA-API version: 1.20"
            first_line
                .split(':')
                .next_back()
                .map_or("unknown", str::trim)
                .to_string()
        }
        "hdr10plus_tool" | "dovi_tool" => first_line
            .split_whitespace()
            .find(|token| {
                token
                    .trim_start_matches('v')
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            })
            .unwrap_or("unknown")
            .trim_start_matches('v')
            .to_string(),
        _ => first_line.to_string(),
    }
}

/// Find the full path to a tool by scanning `PATH`.
fn find_tool_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    find_tool_path_in_paths(name, env::split_paths(&paths))
}

fn find_tool_path_in_paths(
    name: &str,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    paths
        .into_iter()
        .flat_map(|path| candidate_paths(&path, name))
        .find(|path| is_executable_file(path))
}

fn candidate_paths(path: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(windows) && Path::new(name).extension().is_none() {
        let extensions = env::var_os("PATHEXT")
            .map(|value| {
                env::split_paths(&value)
                    .map(|ext| ext.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()]);

        extensions
            .into_iter()
            .map(|ext| path.join(format!("{name}{ext}")))
            .collect()
    } else {
        vec![path.join(name)]
    }
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
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
    fn test_parse_version_hdr_tools() {
        assert_eq!(
            parse_version("hdr10plus_tool", "hdr10plus_tool 1.6.0"),
            "1.6.0"
        );
        assert_eq!(parse_version("dovi_tool", "dovi_tool 2.1.0"), "2.1.0");
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
    fn test_parse_version_nvidia_smi() {
        let output = "NVIDIA-SMI version  : 580.126.18";
        assert_eq!(parse_version("nvidia-smi", output), "580.126.18");
    }

    #[test]
    fn test_parse_version_vainfo() {
        let output = "vainfo: VA-API version: 1.20";
        assert_eq!(parse_version("vainfo", output), "1.20");
    }

    #[test]
    fn test_parse_version_empty() {
        assert_eq!(parse_version("ffprobe", ""), "unknown");
    }

    #[test]
    #[cfg(unix)]
    fn find_tool_path_in_paths_requires_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let tool = dir.path().join("fake-tool");
        std::fs::write(&tool, "#!/bin/sh\n").expect("write tool");

        assert_eq!(
            find_tool_path_in_paths("fake-tool", [dir.path().to_path_buf()]),
            None
        );

        let mut perms = std::fs::metadata(&tool).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool, perms).expect("chmod");

        assert_eq!(
            find_tool_path_in_paths("fake-tool", [dir.path().to_path_buf()]),
            Some(tool)
        );
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
