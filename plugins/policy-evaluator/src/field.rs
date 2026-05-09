//! Field resolution shared by conditions and track filters.

use voom_domain::capability_map::CapabilityMap;
use voom_domain::media::MediaFile;
use voom_domain::plan::PhaseOutput;

/// Closure that resolves a phase name to its persisted `PhaseOutput`.
///
/// Returns `None` when no phase output is recorded for the given name.
/// Callers (CLI, phase orchestrator) populate this from persisted state
/// before evaluating downstream phases.
pub type PhaseOutputLookup<'a> = dyn Fn(&str) -> Option<PhaseOutput> + 'a;

/// Evaluation context carrying system-level information (e.g. hwaccel
/// capabilities) and cross-phase outputs into condition evaluation.
pub struct EvalContext<'a> {
    pub capabilities: Option<&'a CapabilityMap>,
    /// Optional closure that resolves `<phase>.<field>` references to
    /// previously-recorded phase outputs.
    pub phase_output_lookup: Option<&'a PhaseOutputLookup<'a>>,
}

impl<'a> EvalContext<'a> {
    /// Construct a context with neither capabilities nor phase outputs.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            capabilities: None,
            phase_output_lookup: None,
        }
    }

    /// Construct a context with only system capabilities.
    #[must_use]
    pub fn with_capabilities(capabilities: Option<&'a CapabilityMap>) -> Self {
        Self {
            capabilities,
            phase_output_lookup: None,
        }
    }

    /// Construct a context with only a phase-output lookup.
    #[must_use]
    pub fn with_phase_outputs(lookup: &'a PhaseOutputLookup<'a>) -> Self {
        Self {
            capabilities: None,
            phase_output_lookup: Some(lookup),
        }
    }
}

/// Resolve a field path against the media file (and optionally system context
/// or cross-phase outputs).
pub(crate) fn resolve_field(
    file: &MediaFile,
    path: &[String],
    ctx: &EvalContext<'_>,
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }

    match path[0].as_str() {
        "video" => resolve_video_field(file, &path[1..]),
        "audio" => resolve_audio_field(file, &path[1..]),
        "plugin" => resolve_plugin_field(file, &path[1..]),
        "file" => resolve_file_field(file, &path[1..]),
        "system" => resolve_system_field(&path[1..], ctx),
        _ => resolve_phase_field(ctx, &path[0], &path[1..]),
    }
}

/// Resolve `<phase>.<field>` against the cross-phase output lookup.
///
/// Returns `None` when no lookup is configured, the phase is unknown to
/// the lookup, or the requested field is not recognised.
pub(crate) fn resolve_phase_field(
    ctx: &EvalContext<'_>,
    phase: &str,
    path: &[String],
) -> Option<serde_json::Value> {
    if path.len() != 1 {
        return None;
    }
    let lookup = ctx.phase_output_lookup?;
    let out = lookup(phase)?;
    match path[0].as_str() {
        "outcome" => out.outcome.map(serde_json::Value::String),
        "completed" => Some(serde_json::Value::Bool(out.completed)),
        "modified" => Some(serde_json::Value::Bool(out.modified)),
        "error_count" => Some(serde_json::json!(out.error_count)),
        "warning_count" => Some(serde_json::json!(out.warning_count)),
        _ => None,
    }
}

/// Resolve `system.*` fields from the capability map.
pub(crate) fn resolve_system_field(
    path: &[String],
    ctx: &EvalContext<'_>,
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let caps = ctx.capabilities?;
    match path[0].as_str() {
        "hwaccel" => Some(serde_json::Value::String(
            caps.default_parallel_resource()
                .and_then(|resource| resource.strip_prefix("hw:"))
                .unwrap_or("none")
                .to_string(),
        )),
        "has_hwaccel" => Some(serde_json::json!(!caps.hw_accels().is_empty())),
        "hwaccels" => {
            let accels: Vec<serde_json::Value> = caps
                .hw_accels()
                .into_iter()
                .map(serde_json::Value::String)
                .collect();
            Some(serde_json::Value::Array(accels))
        }
        _ => None,
    }
}

pub(crate) fn resolve_video_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    let video = file.video_tracks().into_iter().next()?;
    resolve_track_field(video, path)
}

pub(crate) fn resolve_audio_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    let audio = file.audio_tracks().into_iter().next()?;
    resolve_track_field(audio, path)
}

pub(crate) fn resolve_track_field(
    track: &voom_domain::media::Track,
    path: &[String],
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    match path[0].as_str() {
        "codec" => Some(serde_json::Value::String(track.codec.clone())),
        "language" | "lang" => Some(serde_json::Value::String(track.language.clone())),
        "title" => Some(serde_json::Value::String(track.title.clone())),
        "channels" => track.channels.map(|c| serde_json::json!(c)),
        "width" => track.width.map(|w| serde_json::json!(w)),
        "height" => track.height.map(|h| serde_json::json!(h)),
        "frame_rate" => track.frame_rate.map(|f| serde_json::json!(f)),
        "is_default" => Some(serde_json::json!(track.is_default)),
        "is_forced" => Some(serde_json::json!(track.is_forced)),
        "is_hdr" => Some(serde_json::json!(track.is_hdr)),
        "is_vfr" => Some(serde_json::json!(track.is_vfr)),
        "hdr_format" => track
            .hdr_format
            .as_ref()
            .map(|f| serde_json::Value::String(f.clone())),
        _ => None,
    }
}

pub(crate) fn resolve_plugin_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let plugin_data = file.plugin_metadata.get(&path[0])?;
    let mut current: &serde_json::Value = plugin_data;
    for key in &path[1..] {
        current = current.get(key)?;
    }
    Some(current.clone())
}

pub(crate) fn resolve_file_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    match path[0].as_str() {
        "container" => Some(serde_json::Value::String(file.container.as_str().into())),
        "size" => Some(serde_json::json!(file.size)),
        "duration" => Some(serde_json::json!(file.duration)),
        "path" => Some(serde_json::Value::String(
            file.path.to_string_lossy().into(),
        )),
        "filename" => file
            .path
            .file_name()
            .map(|n| serde_json::Value::String(n.to_string_lossy().into())),
        _ => file
            .tags
            .get(&path[0])
            .map(|v| serde_json::Value::String(v.clone())),
    }
}

/// Resolve a `CompiledValueOrField` to a concrete string value.
#[must_use]
pub fn resolve_value_or_field(
    vof: &voom_dsl::compiled::CompiledValueOrField,
    file: &MediaFile,
    ctx: &EvalContext<'_>,
) -> Option<String> {
    match vof {
        voom_dsl::compiled::CompiledValueOrField::Value(v) => match v {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        },
        voom_dsl::compiled::CompiledValueOrField::Field(path) => resolve_field(file, path, ctx)
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            }),
    }
}
