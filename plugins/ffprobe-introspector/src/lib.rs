//! `FFprobe` introspection plugin: media file analysis via ffprobe JSON output.

pub mod ffprobe;
pub mod parser;

use std::time::Duration;

use voom_domain::errors::Result;
use voom_domain::events::FileIntrospectedEvent;

/// `FFprobe` introspector: extracts media metadata using ffprobe.
///
/// Called directly by the CLI via `introspect()`, not registered with the kernel.
pub struct FfprobeIntrospectorPlugin {
    ffprobe_path: String,
    timeout: Duration,
}

impl FfprobeIntrospectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ffprobe_path: "ffprobe".into(),
            timeout: Duration::from_secs(60),
        }
    }

    /// Set a custom path to the ffprobe binary.
    #[must_use]
    pub fn with_ffprobe_path(mut self, path: impl Into<String>) -> Self {
        self.ffprobe_path = path.into();
        self
    }

    #[must_use]
    pub fn ffprobe_path(&self) -> &str {
        &self.ffprobe_path
    }

    /// Introspect a single file and return the event.
    pub fn introspect(
        &self,
        path: &std::path::Path,
        size: u64,
        content_hash: &str,
    ) -> Result<FileIntrospectedEvent> {
        let json = ffprobe::run_ffprobe(&self.ffprobe_path, path, self.timeout)?;
        let file = parser::parse_ffprobe_output(&json, path, size, content_hash)?;
        Ok(FileIntrospectedEvent::new(file))
    }
}

impl Default for FfprobeIntrospectorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_creates_same_as_new() {
        let plugin = FfprobeIntrospectorPlugin::default();
        assert_eq!(plugin.ffprobe_path(), "ffprobe");
    }

    #[test]
    fn test_custom_ffprobe_path() {
        let plugin = FfprobeIntrospectorPlugin::new().with_ffprobe_path("/usr/local/bin/ffprobe");
        assert_eq!(plugin.ffprobe_path(), "/usr/local/bin/ffprobe");
    }
}
