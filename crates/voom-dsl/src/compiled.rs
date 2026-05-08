//! Compiled policy IR types.
//!
//! These are the intermediate representation types produced by the DSL compiler
//! and consumed by the policy evaluator and phase orchestrator plugins.

use std::collections::HashMap;
use std::fmt;

use regex::Regex;
use serde::{Deserialize, Serialize};
pub use voom_domain::media::Container;
pub use voom_domain::plan::{SampleStrategy, TranscodeChannels, TranscodeFallback};

/// A pre-compiled regex that supports `Clone`, `Debug`, `Serialize`, and `Deserialize`.
///
/// Serialized as the pattern string; deserialization re-compiles the regex.
#[derive(Clone)]
#[non_exhaustive]
pub struct CompiledRegex {
    pattern: String,
    regex: Regex,
}

impl CompiledRegex {
    /// Compile a new regex from the given pattern.
    ///
    /// # Errors
    ///
    /// Returns `regex::Error` if the pattern is not a valid regular expression.
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        let regex = Regex::new(pattern)?;
        Ok(Self {
            pattern: pattern.to_string(),
            regex,
        })
    }

    /// Returns the underlying compiled `Regex`.
    #[must_use]
    pub fn regex(&self) -> &Regex {
        &self.regex
    }

    /// Returns the original pattern string.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

impl fmt::Debug for CompiledRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CompiledRegex").field(&self.pattern).finish()
    }
}

impl Serialize for CompiledRegex {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.pattern.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CompiledRegex {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pattern = String::deserialize(deserializer)?;
        CompiledRegex::new(&pattern).map_err(serde::de::Error::custom)
    }
}

/// A compiled policy ready for evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledPolicy {
    pub name: String,
    pub config: CompiledConfig,
    pub phases: Vec<CompiledPhase>,
    /// Topologically sorted phase execution order.
    pub phase_order: Vec<String>,
    /// xxHash64 of the policy source text.
    #[serde(default)]
    pub source_hash: String,
}

impl CompiledPolicy {
    #[must_use]
    pub fn new(
        name: String,
        config: CompiledConfig,
        phases: Vec<CompiledPhase>,
        phase_order: Vec<String>,
        source_hash: String,
    ) -> Self {
        Self {
            name,
            config,
            phases,
            phase_order,
            source_hash,
        }
    }
}

/// Compiled configuration block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledConfig {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub on_error: ErrorStrategy,
    pub commentary_patterns: Vec<String>,
    #[serde(default)]
    pub keep_backups: bool,
}

impl CompiledConfig {
    #[must_use]
    pub fn new(
        audio_languages: Vec<String>,
        subtitle_languages: Vec<String>,
        on_error: ErrorStrategy,
        commentary_patterns: Vec<String>,
        keep_backups: bool,
    ) -> Self {
        Self {
            audio_languages,
            subtitle_languages,
            on_error,
            commentary_patterns,
            keep_backups,
        }
    }
}

/// Error handling strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorStrategy {
    Continue,
    Abort,
    Skip,
    Quarantine,
}

/// A compiled phase with resolved references.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledPhase {
    pub name: String,
    pub depends_on: Vec<String>,
    pub skip_when: Option<CompiledCondition>,
    pub run_if: Option<CompiledRunIf>,
    pub on_error: ErrorStrategy,
    pub operations: Vec<CompiledOperation>,
}

/// Compiled `run_if` trigger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledRunIf {
    pub phase: String,
    pub trigger: RunIfTrigger,
}

impl CompiledRunIf {
    #[must_use]
    pub fn new(phase: String, trigger: RunIfTrigger) -> Self {
        Self { phase, trigger }
    }
}

/// Run-if trigger type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunIfTrigger {
    Modified,
    Completed,
}

/// A compiled operation within a phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
// Keep compiled transcode settings inline so evaluator consumers can continue
// pattern-matching the existing IR without allocation or API churn.
#[allow(clippy::large_enum_variant)]
pub enum CompiledOperation {
    SetContainer(Container),
    Keep {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    Remove {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    ReorderTracks(Vec<String>),
    SetDefaults(Vec<CompiledDefault>),
    ClearActions {
        target: TrackTarget,
        settings: ClearActionsSettings,
    },
    Transcode {
        target: TrackTarget,
        codec: String,
        settings: CompiledTranscodeSettings,
    },
    Synthesize(Box<CompiledSynthesize>),
    ClearTags,
    SetTag {
        tag: String,
        value: CompiledValueOrField,
    },
    DeleteTag(String),
    Conditional(CompiledConditional),
    Rules {
        mode: RulesMode,
        rules: Vec<CompiledRule>,
    },
    Verify {
        mode: voom_domain::verification::VerificationMode,
    },
}

/// Track target category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackTarget {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Any,
}

/// Defaults strategy for a track type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledDefault {
    pub target: TrackTarget,
    pub strategy: DefaultStrategy,
}

impl CompiledDefault {
    #[must_use]
    pub fn new(target: TrackTarget, strategy: DefaultStrategy) -> Self {
        Self { target, strategy }
    }
}

/// Default track selection strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefaultStrategy {
    FirstPerLanguage,
    None,
    First,
    All,
}

/// Settings for the `ClearActions` operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct ClearActionsSettings {
    pub clear_all_default: bool,
    pub clear_all_forced: bool,
    pub clear_all_titles: bool,
}

impl ClearActionsSettings {
    #[must_use]
    pub fn new(clear_all_default: bool, clear_all_forced: bool, clear_all_titles: bool) -> Self {
        Self {
            clear_all_default,
            clear_all_forced,
            clear_all_titles,
        }
    }
}

/// Settings for the `Transcode` operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct CompiledTranscodeSettings {
    pub preserve: Vec<String>,
    pub crf: Option<u32>,
    pub preset: Option<String>,
    pub bitrate: Option<String>,
    pub target_vmaf: Option<u32>,
    pub max_bitrate: Option<String>,
    pub min_bitrate: Option<String>,
    pub sample_strategy: Option<SampleStrategy>,
    pub fallback: Option<TranscodeFallback>,
    pub vmaf_overrides: Option<HashMap<String, u32>>,
    pub channels: Option<TranscodeChannels>,
    /// Hardware acceleration backend preference.
    /// Values: "auto", "nvenc", "qsv", "vaapi", "videotoolbox", "none".
    #[serde(default)]
    pub hw: Option<String>,
    /// Whether to fall back to software encoding when the requested
    /// HW backend is unavailable. Defaults to `true` when absent.
    #[serde(default)]
    pub hw_fallback: Option<bool>,
    /// Maximum resolution (e.g. "1080p", "4k"). Downscale if source exceeds.
    #[serde(default)]
    pub max_resolution: Option<String>,
    /// Scaling algorithm (e.g. "lanczos", "bicubic", "bilinear").
    #[serde(default)]
    pub scale_algorithm: Option<String>,
    /// HDR handling mode (e.g. "preserve", "tonemap").
    #[serde(default)]
    pub hdr_mode: Option<String>,
    /// Encoder tuning hint (e.g. "film", "animation", "grain").
    #[serde(default)]
    pub tune: Option<String>,
}

impl CompiledTranscodeSettings {
    #[must_use]
    pub fn new(
        preserve: Vec<String>,
        crf: Option<u32>,
        preset: Option<String>,
        bitrate: Option<String>,
        channels: Option<TranscodeChannels>,
    ) -> Self {
        Self {
            preserve,
            crf,
            preset,
            bitrate,
            target_vmaf: None,
            max_bitrate: None,
            min_bitrate: None,
            sample_strategy: None,
            fallback: None,
            vmaf_overrides: None,
            channels,
            hw: None,
            hw_fallback: None,
            max_resolution: None,
            scale_algorithm: None,
            hdr_mode: None,
            tune: None,
        }
    }
}

/// Position hint for a synthesize operation — either a named position or a numeric index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SynthPosition {
    /// Named position, e.g. `after_source`, `last`.
    Named(String),
    /// Zero-based track index.
    Index(u32),
}

/// Synthesize operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledSynthesize {
    pub name: String,
    pub codec: Option<String>,
    pub channels: Option<TranscodeChannels>,
    pub source: Option<CompiledFilter>,
    pub bitrate: Option<String>,
    pub skip_if_exists: Option<CompiledFilter>,
    pub create_if: Option<CompiledCondition>,
    pub title: Option<String>,
    pub language: Option<SynthLanguage>,
    pub position: Option<SynthPosition>,
}

impl CompiledSynthesize {
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            name,
            codec: None,
            channels: None,
            source: None,
            bitrate: None,
            skip_if_exists: None,
            create_if: None,
            title: None,
            language: None,
            position: None,
        }
    }
}

/// Language setting for synthesize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SynthLanguage {
    Inherit,
    Fixed(String),
}

/// A conditional (when/else) block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledConditional {
    pub condition: CompiledCondition,
    pub then_actions: Vec<CompiledAction>,
    pub else_actions: Vec<CompiledAction>,
}

impl CompiledConditional {
    #[must_use]
    pub fn new(
        condition: CompiledCondition,
        then_actions: Vec<CompiledAction>,
        else_actions: Vec<CompiledAction>,
    ) -> Self {
        Self {
            condition,
            then_actions,
            else_actions,
        }
    }
}

/// A named rule within a rules block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompiledRule {
    pub name: String,
    pub conditional: CompiledConditional,
}

impl CompiledRule {
    #[must_use]
    pub fn new(name: String, conditional: CompiledConditional) -> Self {
        Self { name, conditional }
    }
}

/// Rules evaluation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RulesMode {
    First,
    All,
}

/// A compiled condition expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledCondition {
    Exists {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    Count {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
        op: CompiledCompareOp,
        value: f64,
    },
    FieldCompare {
        path: Vec<String>,
        op: CompiledCompareOp,
        value: serde_json::Value,
    },
    FieldExists {
        path: Vec<String>,
    },
    AudioIsMultiLanguage,
    IsDubbed,
    IsOriginal,
    And(Vec<CompiledCondition>),
    Or(Vec<CompiledCondition>),
    Not(Box<CompiledCondition>),
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompiledCompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

/// A compiled filter expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledFilter {
    LangIn(Vec<String>),
    LangCompare(CompiledCompareOp, String),
    LangField(CompiledCompareOp, Vec<String>),
    CodecIn(Vec<String>),
    CodecCompare(CompiledCompareOp, String),
    CodecField(CompiledCompareOp, Vec<String>),
    Channels(CompiledCompareOp, f64),
    Commentary,
    Forced,
    Default,
    Font,
    TitleContains(String),
    TitleMatches(CompiledRegex),
    And(Vec<CompiledFilter>),
    Or(Vec<CompiledFilter>),
    Not(Box<CompiledFilter>),
}

/// A compiled action within a conditional block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledAction {
    Keep {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    Remove {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    Skip(Option<String>),
    Warn(String),
    Fail(String),
    SetDefault {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    SetForced {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
    },
    SetLanguage {
        target: TrackTarget,
        filter: Option<CompiledFilter>,
        value: CompiledValueOrField,
    },
    SetTag {
        tag: String,
        value: CompiledValueOrField,
    },
}

/// Either a literal value or a field access path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompiledValueOrField {
    Value(serde_json::Value),
    Field(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- CompiledRegex::pattern accessor (issue #236, cluster C) ----

    #[test]
    fn compiled_regex_pattern_returns_input() {
        // Kills both `pattern -> &str` mutants ("" and "xyzzy"): the original
        // returns the exact input string, not a constant placeholder.
        let r = CompiledRegex::new("hello.*world").unwrap();
        assert_eq!(r.pattern(), "hello.*world");
    }

    #[test]
    fn compiled_regex_pattern_distinct_from_mutant_constants() {
        // Belt-and-suspenders: explicitly assert the value is neither of the
        // two constants cargo-mutants substitutes.
        let r = CompiledRegex::new("commentary").unwrap();
        let pat = r.pattern();
        assert_ne!(pat, "");
        assert_ne!(pat, "xyzzy");
        assert_eq!(pat, "commentary");
    }

    #[test]
    fn compiled_regex_serializes_as_pattern_string() {
        // Serde uses the pattern field directly. Guards against silent drift
        // if Serialize is ever rewritten to emit the compiled regex shape.
        let r = CompiledRegex::new(r"\d+").unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#""\\d+""#);
    }

    #[test]
    fn compiled_regex_serde_roundtrip_preserves_pattern() {
        let original = CompiledRegex::new(r"foo\d+bar").unwrap();
        let json = serde_json::to_string(&original).unwrap();
        let decoded: CompiledRegex = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pattern(), original.pattern());
        // Verify the regex still matches after roundtrip.
        assert!(decoded.regex().is_match("foo42bar"));
    }
}
