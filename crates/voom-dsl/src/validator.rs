//! Semantic validation for VOOM DSL ASTs.
//!
//! The `.unwrap()` calls in this module operate on structures that are guaranteed
//! to exist by construction (e.g., cycle detection paths, AST lookups).
#![allow(clippy::unwrap_used)]
//!
//! Checks for:
//! - Unknown codecs (with did-you-mean suggestions)
//! - Invalid language codes
//! - Circular phase dependencies
//! - Unreachable phases
//! - Conflicting track actions (keep + remove on same target)
//! - Invalid phase references in `depends_on` and `run_if`
//! - Invalid container names

use std::collections::{HashMap, HashSet};

use voom_domain::utils::{codecs, codecs::CodecType, language};

use crate::ast::{
    ActionNode, ConditionNode, FilterNode, OperationNode, PhaseNode, PolicyAst, SynthSetting,
    Value, ValueOrField, WhenNode,
};
use crate::errors::{DslError, DslWarning, ValidationErrors};

/// Validate a parsed AST for semantic correctness.
/// Returns `Ok(())` if valid, or `Err(ValidationErrors)` with all errors found.
///
/// # Examples
///
/// ```
/// use voom_dsl::{parse_policy, validate};
///
/// let ast = parse_policy(r#"policy "demo" {
///     phase init {
///         container mkv
///     }
/// }"#).unwrap();
///
/// assert!(validate(&ast).is_ok());
/// ```
pub fn validate(ast: &PolicyAst) -> std::result::Result<(), ValidationErrors> {
    let (_warnings, result) = validate_collecting_warnings(ast);
    result
}

/// Validate a parsed AST, returning both warnings and errors.
///
/// Warnings are non-fatal issues (e.g., unknown plugin names in field paths).
/// Errors are fatal validation failures. The result follows the same contract
/// as [`validate`]: `Ok(())` if no errors, `Err(ValidationErrors)` otherwise.
pub fn validate_with_warnings(
    ast: &PolicyAst,
) -> (Vec<DslWarning>, std::result::Result<(), ValidationErrors>) {
    validate_collecting_warnings(ast)
}

const KNOWN_PLUGIN_NAMES: &[&str] = &["radarr", "sonarr", "plex"];

fn validate_collecting_warnings(
    ast: &PolicyAst,
) -> (Vec<DslWarning>, std::result::Result<(), ValidationErrors>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    validate_config(ast, &mut errors);
    validate_phase_names(ast, &mut errors);
    validate_phase_references(ast, &mut errors);
    validate_cycle_detection(ast, &mut errors);
    validate_reachability(ast, &mut errors);

    let phase_names: HashSet<&str> = ast.phases.iter().map(|p| p.name.as_str()).collect();

    for phase in &ast.phases {
        validate_phase(phase, &phase_names, &mut errors, &mut warnings);
    }

    let result = if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors { errors })
    };
    (warnings, result)
}

fn validate_config(ast: &PolicyAst, errors: &mut Vec<DslError>) {
    let Some(config) = &ast.config else { return };

    for lang in &config.audio_languages {
        if !language::is_valid_language(lang) {
            errors.push(DslError::validation(
                config.span.line,
                config.span.col,
                format!("unknown audio language code \"{lang}\""),
            ));
        }
    }
    for lang in &config.subtitle_languages {
        if !language::is_valid_language(lang) {
            errors.push(DslError::validation(
                config.span.line,
                config.span.col,
                format!("unknown subtitle language code \"{lang}\""),
            ));
        }
    }
}

fn validate_phase_names(ast: &PolicyAst, errors: &mut Vec<DslError>) {
    let mut seen = HashSet::new();
    for phase in &ast.phases {
        if !seen.insert(&phase.name) {
            errors.push(DslError::validation(
                phase.span.line,
                phase.span.col,
                format!("duplicate phase name \"{}\"", phase.name),
            ));
        }
    }
}

fn validate_phase_references(ast: &PolicyAst, errors: &mut Vec<DslError>) {
    let phase_names: HashSet<&str> = ast.phases.iter().map(|p| p.name.as_str()).collect();

    for phase in &ast.phases {
        for dep in &phase.depends_on {
            if !phase_names.contains(dep.as_str()) {
                errors.push(DslError::validation(
                    phase.span.line,
                    phase.span.col,
                    format!(
                        "phase \"{}\" depends on unknown phase \"{}\"",
                        phase.name, dep
                    ),
                ));
            }
            if dep == &phase.name {
                errors.push(DslError::validation(
                    phase.span.line,
                    phase.span.col,
                    format!("phase \"{}\" depends on itself", phase.name),
                ));
            }
        }

        if let Some(run_if) = &phase.run_if {
            if !phase_names.contains(run_if.phase.as_str()) {
                errors.push(DslError::validation(
                    phase.span.line,
                    phase.span.col,
                    format!(
                        "phase \"{}\" has run_if referencing unknown phase \"{}\"",
                        phase.name, run_if.phase
                    ),
                ));
            }
        }
    }
}

fn validate_cycle_detection(ast: &PolicyAst, errors: &mut Vec<DslError>) {
    // Build adjacency list
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for phase in &ast.phases {
        let deps: Vec<&str> = phase
            .depends_on
            .iter()
            .map(std::string::String::as_str)
            .collect();
        adj.insert(phase.name.as_str(), deps);
    }

    // DFS cycle detection
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();

    for phase in &ast.phases {
        if !visited.contains(phase.name.as_str()) {
            let mut path = Vec::new();
            if detect_cycle(
                phase.name.as_str(),
                &adj,
                &mut visited,
                &mut in_stack,
                &mut path,
            ) {
                // Find the cycle start in path
                let cycle_start = path.last().unwrap();
                let cycle_pos = path.iter().position(|p| p == cycle_start).unwrap();
                let cycle: Vec<&str> = path[cycle_pos..].to_vec();
                let cycle_str = cycle.join(" → ");

                let span = &ast
                    .phases
                    .iter()
                    .find(|p| p.name == *cycle_start)
                    .unwrap()
                    .span;
                errors.push(DslError::validation(
                    span.line,
                    span.col,
                    format!("circular dependency: {cycle_str}"),
                ));
                in_stack.clear();
            }
        }
    }
}

fn detect_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
    in_stack: &mut HashSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> bool {
    visited.insert(node);
    in_stack.insert(node);
    path.push(node);

    if let Some(deps) = adj.get(node) {
        for &dep in deps {
            if !visited.contains(dep) {
                if detect_cycle(dep, adj, visited, in_stack, path) {
                    return true;
                }
            } else if in_stack.contains(dep) {
                path.push(dep);
                return true;
            }
        }
    }

    in_stack.remove(node);
    path.pop();
    false
}

fn validate_reachability(ast: &PolicyAst, errors: &mut Vec<DslError>) {
    if ast.phases.is_empty() {
        return;
    }

    // Phases are reachable if they have no dependencies (roots) or if all
    // their dependencies are reachable. A phase with a depends_on referencing
    // an unknown phase is already flagged; here we check for phases that are
    // not reachable from any root.
    let phase_names: HashSet<&str> = ast.phases.iter().map(|p| p.name.as_str()).collect();
    let mut reachable: HashSet<&str> = HashSet::new();

    // Roots: phases with no depends_on
    for phase in &ast.phases {
        if phase.depends_on.is_empty() {
            reachable.insert(phase.name.as_str());
        }
    }

    // Fixed-point: mark reachable transitively
    let mut changed = true;
    while changed {
        changed = false;
        for phase in &ast.phases {
            if reachable.contains(phase.name.as_str()) {
                continue;
            }
            let all_deps_reachable = phase
                .depends_on
                .iter()
                .all(|d| !phase_names.contains(d.as_str()) || reachable.contains(d.as_str()));
            if all_deps_reachable {
                reachable.insert(phase.name.as_str());
                changed = true;
            }
        }
    }

    for phase in &ast.phases {
        if !reachable.contains(phase.name.as_str()) {
            errors.push(DslError::validation(
                phase.span.line,
                phase.span.col,
                format!("phase \"{}\" is unreachable", phase.name),
            ));
        }
    }
}

fn validate_phase(
    phase: &PhaseNode,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    if let Some(skip_when) = &phase.skip_when {
        validate_condition(
            skip_when,
            phase.span.line,
            phase.span.col,
            phase_names,
            errors,
            warnings,
        );
    }

    // Track keep/remove conflicts (target, has_filter)
    let mut kept_targets: Vec<(&str, bool)> = Vec::new();
    let mut removed_targets: Vec<(&str, bool)> = Vec::new();

    for spanned_op in &phase.operations {
        validate_operation(
            &spanned_op.node,
            spanned_op.span.line,
            spanned_op.span.col,
            phase_names,
            errors,
            warnings,
        );

        match &spanned_op.node {
            OperationNode::Keep { target, filter, .. } => {
                kept_targets.push((target.as_str(), filter.is_some()));
            }
            OperationNode::Remove { target, filter } => {
                removed_targets.push((target.as_str(), filter.is_some()));
            }
            _ => {}
        }
    }

    check_tag_conflicts(phase, errors);
    check_track_conflicts(phase, &kept_targets, &removed_targets, errors);
}

fn check_tag_conflicts(phase: &PhaseNode, errors: &mut Vec<DslError>) {
    let mut set_tag_keys: HashSet<&str> = HashSet::new();
    let mut delete_tag_keys: HashSet<&str> = HashSet::new();
    let mut clear_tags_index: Option<usize> = None;

    for (i, spanned_op) in phase.operations.iter().enumerate() {
        match &spanned_op.node {
            OperationNode::ClearTags => {
                clear_tags_index = Some(i);
            }
            OperationNode::SetTag { tag, .. } => {
                set_tag_keys.insert(tag.as_str());
            }
            OperationNode::DeleteTag(tag) => {
                delete_tag_keys.insert(tag.as_str());
            }
            _ => {}
        }
    }

    for key in &set_tag_keys {
        if delete_tag_keys.contains(key) {
            errors.push(DslError::validation(
                phase.span.line,
                phase.span.col,
                format!(
                    "tag \"{key}\" is both set and deleted in phase \"{}\"",
                    phase.name
                ),
            ));
        }
    }

    // Warn if set_tag appears before clear_tags (clear will undo the set)
    if let Some(clear_idx) = clear_tags_index {
        if !set_tag_keys.is_empty() {
            for (i, spanned_op) in phase.operations.iter().enumerate() {
                if let OperationNode::SetTag { tag, .. } = &spanned_op.node {
                    if i < clear_idx {
                        errors.push(DslError::validation(
                            spanned_op.span.line,
                            spanned_op.span.col,
                            format!(
                                "set_tag \"{tag}\" appears before clear_tags and will be overwritten"
                            ),
                        ));
                    }
                }
            }
        }
    }
}

fn check_track_conflicts(
    phase: &PhaseNode,
    kept_targets: &[(&str, bool)],
    removed_targets: &[(&str, bool)],
    errors: &mut Vec<DslError>,
) {
    for &(target, keep_filtered) in kept_targets {
        let broad_category = broad_track_category(target);
        for &(removed, remove_filtered) in removed_targets {
            if broad_track_category(removed) == broad_category && !keep_filtered && !remove_filtered
            {
                errors.push(DslError::validation(
                    phase.span.line,
                    phase.span.col,
                    format!(
                        "conflicting keep and remove on \"{}\" in phase \"{}\"",
                        broad_category, phase.name
                    ),
                ));
            }
        }
    }
}

fn broad_track_category(target: &str) -> &str {
    match target {
        "video" => "video",
        "audio" | "audio_main" | "audio_alternate" | "audio_commentary" => "audio",
        "subtitle" | "subtitles" | "subtitle_main" | "subtitle_forced" | "subtitle_commentary" => {
            "subtitle"
        }
        "attachment" | "attachments" => "attachment",
        _ => target,
    }
}

#[allow(clippy::too_many_lines)] // Dispatch over all operation variants; splitting would fragment validation logic.
fn validate_operation(
    op: &OperationNode,
    line: usize,
    col: usize,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    match op {
        OperationNode::Container(name) => {
            if voom_domain::Container::from_extension(name) == voom_domain::Container::Other {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!(
                        "unknown container '{name}'; expected one of: {}",
                        voom_domain::Container::known_extensions().join(", ")
                    ),
                ));
            }
        }
        OperationNode::Keep { target, filter, .. } | OperationNode::Remove { target, filter } => {
            validate_track_target(target, line, col, errors);
            if let Some(f) = filter {
                validate_filter(f, line, col, phase_names, errors, warnings);
            }
        }
        OperationNode::Transcode {
            target,
            codec,
            settings,
        } => {
            validate_transcode_operation(target, codec, settings, line, col, errors, warnings);
        }
        OperationNode::Synthesize { settings, .. } => {
            validate_synthesize_operation(settings, line, col, phase_names, errors, warnings);
        }
        OperationNode::When(when) => {
            validate_when(when, phase_names, errors, warnings);
        }
        OperationNode::Rules { mode, rules } => {
            match mode.as_str() {
                "first" | "all" => {}
                _ => {
                    errors.push(DslError::validation(
                        line,
                        col,
                        format!("invalid rules mode \"{mode}\", expected \"first\" or \"all\""),
                    ));
                }
            }
            for rule in rules {
                validate_when(&rule.when, phase_names, errors, warnings);
            }
        }
        OperationNode::Order(items) => {
            let valid_order_items = [
                "video",
                "audio",
                "audio_main",
                "audio_alternate",
                "audio_commentary",
                "subtitle",
                "subtitles",
                "subtitle_main",
                "subtitle_forced",
                "subtitle_commentary",
                "attachment",
            ];
            for item in items {
                if !valid_order_items.contains(&item.as_str()) {
                    errors.push(DslError::validation(
                        line,
                        col,
                        format!("unknown track order item \"{item}\""),
                    ));
                }
            }
        }
        OperationNode::Defaults(items) => {
            for (kind, value) in items {
                match kind.as_str() {
                    "audio" | "subtitle" => {}
                    _ => {
                        errors.push(DslError::validation(
                            line,
                            col,
                            format!("invalid defaults kind \"{kind}\""),
                        ));
                    }
                }
                if crate::compiler::parse_default_strategy(value).is_none() {
                    errors.push(DslError::validation(
                        line,
                        col,
                        format!(
                            "invalid defaults value \"{value}\", expected one of: first_per_language, none, first, all"
                        ),
                    ));
                }
            }
        }
        OperationNode::Actions { target, settings } => {
            validate_track_target(target, line, col, errors);
            validate_actions_settings(settings, line, col, errors);
        }
        OperationNode::ClearTags => {}
        OperationNode::SetTag { tag, .. } => {
            if tag.is_empty() {
                errors.push(DslError::validation(
                    line,
                    col,
                    "set_tag requires a non-empty tag name".to_string(),
                ));
            }
        }
        OperationNode::DeleteTag(tag) => {
            if tag.is_empty() {
                errors.push(DslError::validation(
                    line,
                    col,
                    "delete_tag requires a non-empty tag name".to_string(),
                ));
            }
        }
        OperationNode::Verify { .. } => {
            // Verify mode is constrained by the grammar; no additional validation needed.
        }
    }
}

fn validate_transcode_operation(
    target: &str,
    codec: &str,
    settings: &[(String, Value)],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    validate_track_target(target, line, col, errors);
    validate_codec(codec, line, col, errors);
    validate_codec_track_type(target, codec, line, col, errors);
    for (_, val) in settings {
        validate_value(val, line, col, errors);
    }
    validate_transcode_keys(settings, line, col, errors);
    validate_hw_settings(settings, line, col, errors);
    if target == "video" {
        validate_video_transcode_settings(settings, line, col, errors);
        validate_vmaf_transcode_settings(settings, line, col, errors, warnings);
    } else {
        reject_video_only_keys(settings, target, line, col, errors);
    }
}

fn validate_synthesize_operation(
    settings: &[SynthSetting],
    line: usize,
    col: usize,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    for setting in settings {
        match setting {
            SynthSetting::Codec(c) => validate_codec(c, line, col, errors),
            SynthSetting::Language(lang) => {
                if lang != "inherit" && !language::is_valid_language(lang) {
                    errors.push(DslError::validation(
                        line,
                        col,
                        format!("unknown language code \"{lang}\""),
                    ));
                }
            }
            SynthSetting::Source(f) | SynthSetting::SkipIfExists(f) => {
                validate_filter(f, line, col, phase_names, errors, warnings);
            }
            SynthSetting::Channels(v) => {
                validate_synth_channels(v, line, col, errors);
            }
            SynthSetting::Position(v) => {
                validate_synth_position(v, line, col, errors);
            }
            SynthSetting::Normalize(setting) => {
                validate_normalize_setting(setting, line, col, errors);
            }
            SynthSetting::Bitrate(_) | SynthSetting::Title(_) | SynthSetting::CreateIf(_) => {}
        }
    }
}

fn validate_normalize_setting(
    setting: &crate::ast::NormalizeSetting,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    if voom_domain::plan::LoudnessPreset::parse(&setting.preset).is_none() {
        errors.push(DslError::validation(
            line,
            col,
            format!("unknown loudness preset \"{}\"", setting.preset),
        ));
    }
    for (key, value) in &setting.settings {
        match key.as_str() {
            "target_lufs" | "true_peak_db" | "lra_max" | "tolerance_lufs" => {
                if !matches!(value, Value::Number(_, _)) {
                    errors.push(DslError::validation(
                        line,
                        col,
                        format!("{key} must be a number"),
                    ));
                }
            }
            _ => errors.push(DslError::validation(
                line,
                col,
                format!("unknown normalize setting \"{key}\""),
            )),
        }
    }
}

fn validate_track_target(target: &str, line: usize, col: usize, errors: &mut Vec<DslError>) {
    let valid = [
        "video",
        "audio",
        "subtitle",
        "subtitles",
        "attachment",
        "attachments",
        "track",
    ];
    if !valid.contains(&target) {
        errors.push(DslError::validation(
            line,
            col,
            format!("unknown track target \"{target}\""),
        ));
    }
}

fn validate_codec(codec: &str, line: usize, col: usize, errors: &mut Vec<DslError>) {
    if codecs::normalize_codec(codec).is_none() {
        if let Some(suggestion) = codecs::suggest_codec(codec) {
            errors.push(DslError::validation_with_suggestion(
                line,
                col,
                format!("unknown codec \"{codec}\""),
                format!("did you mean \"{suggestion}\"?"),
            ));
        } else {
            errors.push(DslError::validation(
                line,
                col,
                format!("unknown codec \"{codec}\""),
            ));
        }
    }
}

/// Known root segments for field access paths in filter expressions.
const KNOWN_FIELD_ROOTS: &[&str] = &["plugin", "file", "video", "audio", "system"];

fn validate_field_path(
    path: &[String],
    line: usize,
    col: usize,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    if path.is_empty() {
        errors.push(DslError::validation(
            line,
            col,
            "empty field path in filter".to_string(),
        ));
        return;
    }
    let head = path[0].as_str();
    let is_known_root = KNOWN_FIELD_ROOTS.contains(&head);
    let is_phase_ref = phase_names.contains(head);

    if !is_known_root && !is_phase_ref {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "unknown field root \"{}\" in filter; \
                 expected one of: {} or a declared phase name",
                path[0],
                KNOWN_FIELD_ROOTS.join(", ")
            ),
        ));
        return;
    }

    if is_phase_ref && !is_known_root {
        validate_phase_output_path(path, line, col, errors);
        return;
    }

    if path[0] == "plugin" && path.len() >= 2 {
        let plugin_name = &path[1];
        if !KNOWN_PLUGIN_NAMES.contains(&plugin_name.as_str()) {
            // Only warn when the name is close to a known plugin (likely typo).
            // Names far from all known plugins are assumed to be WASM plugins
            // and should not trigger a warning.
            if let Some(suggestion) = suggest_from(plugin_name, KNOWN_PLUGIN_NAMES) {
                warnings.push(DslWarning::new(
                    line,
                    col,
                    format!(
                        "unknown plugin name \"{plugin_name}\" in field path; \
                         known plugins: {}",
                        KNOWN_PLUGIN_NAMES.join(", ")
                    ),
                    Some(format!("did you mean \"{suggestion}\"?")),
                ));
            }
        }
    }
}

/// Validate that a `<phase>.<field>` reference uses a recognised phase
/// output field. Trailing path beyond the first segment is not allowed
/// (phase outputs are flat).
fn validate_phase_output_path(
    path: &[String],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    if path.len() < 2 {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "phase reference \"{}\" requires a field; \
                 expected one of: {}",
                path[0],
                voom_domain::PHASE_OUTPUT_FIELDS.join(", "),
            ),
        ));
        return;
    }
    if path.len() > 2 {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "phase output \"{}.{}\" does not support nested fields",
                path[0], path[1]
            ),
        ));
        return;
    }
    let field = path[1].as_str();
    if !voom_domain::PHASE_OUTPUT_FIELDS.contains(&field) {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "unknown phase output field \"{}.{}\"; expected one of: {}",
                path[0],
                field,
                voom_domain::PHASE_OUTPUT_FIELDS.join(", "),
            ),
        ));
    }
}

fn validate_filter(
    filter: &FilterNode,
    line: usize,
    col: usize,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    match filter {
        FilterNode::LangIn(langs) => {
            for lang in langs {
                if !language::is_valid_language(lang) {
                    errors.push(DslError::validation(
                        line,
                        col,
                        format!("unknown language code \"{lang}\" in filter"),
                    ));
                }
            }
        }
        FilterNode::LangCompare(_, lang) => {
            if !language::is_valid_language(lang) {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!("unknown language code \"{lang}\" in filter"),
                ));
            }
        }
        FilterNode::LangField(_, path) | FilterNode::CodecField(_, path) => {
            validate_field_path(path, line, col, phase_names, errors, warnings);
        }
        FilterNode::CodecIn(codecs_list) => {
            for codec in codecs_list {
                validate_codec(codec, line, col, errors);
            }
        }
        FilterNode::CodecCompare(_, codec) => {
            validate_codec(codec, line, col, errors);
        }
        FilterNode::And(items) | FilterNode::Or(items) => {
            for item in items {
                validate_filter(item, line, col, phase_names, errors, warnings);
            }
        }
        FilterNode::TitleMatches(pattern) => {
            if regex::Regex::new(pattern).is_err() {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!("invalid regex pattern in title matches: \"{pattern}\""),
                ));
            }
        }
        FilterNode::Not(inner) => validate_filter(inner, line, col, phase_names, errors, warnings),
        _ => {}
    }
}

fn validate_value(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    if let Value::Number(_, raw) = val {
        validate_number_suffix(raw, line, col, errors);
    }
}

fn has_number_suffix(raw: &str) -> bool {
    raw.chars()
        .last()
        .is_some_and(|c| !c.is_ascii_digit() && c != '.')
}

fn validate_number_suffix(raw: &str, line: usize, col: usize, errors: &mut Vec<DslError>) {
    let suffix: String = raw
        .chars()
        .skip_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if suffix.is_empty() {
        return;
    }
    let valid_suffixes = ["k", "K", "p", "m", "M", "g", "G"];
    if !valid_suffixes.contains(&suffix.as_str()) {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "unknown number suffix \"{suffix}\" in \"{raw}\", expected one of: {}",
                valid_suffixes.join(", ")
            ),
        ));
    }
}

const KNOWN_TRANSCODE_KEYS: &[&str] = &[
    "preserve",
    "crf",
    "preset",
    "bitrate",
    "target_vmaf",
    "max_bitrate",
    "min_bitrate",
    "sample_strategy",
    "fallback",
    "channels",
    "hw",
    "hw_fallback",
    "max_resolution",
    "scale_algorithm",
    "hdr_mode",
    "preserve_hdr",
    "tonemap",
    "hdr_color_metadata",
    "dolby_vision",
    "tune",
    "crop",
    "crop_sample_duration",
    "crop_sample_count",
    "crop_threshold",
    "crop_minimum",
    "crop_preserve_bottom_pixels",
    "crop_aspect_lock",
    "normalize",
];

fn validate_transcode_keys(
    settings: &[(String, Value)],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    for (key, _) in settings {
        if key.len() > 64 {
            errors.push(DslError::validation(
                line,
                col,
                format!("transcode setting key too long: \"{key}\""),
            ));
            continue;
        }
        if key.starts_with("target_vmaf_when ") {
            continue;
        }
        validate_ident_against(
            key,
            "transcode setting",
            KNOWN_TRANSCODE_KEYS,
            line,
            col,
            errors,
        );
    }
}

fn validate_codec_track_type(
    target: &str,
    codec: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    let expected_type = match target {
        "video" => CodecType::Video,
        "audio" => CodecType::Audio,
        // Grammar constrains transcode targets to "video" | "audio"
        _ => return,
    };
    let Some(actual_type) = codecs::codec_type(codec) else {
        return; // Unknown codec already reported by validate_codec
    };
    if actual_type != expected_type {
        let type_name = match actual_type {
            CodecType::Video => "video",
            CodecType::Audio => "audio",
            CodecType::Subtitle => "subtitle",
        };
        let article = if type_name.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        };
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "codec \"{codec}\" is {article} {type_name} codec \
                 but target is {target}"
            ),
        ));
    }
}

const VALID_HW_VALUES: &[&str] = &["auto", "nvenc", "qsv", "vaapi", "videotoolbox", "none"];

/// Find the best did-you-mean suggestion from `known` values for `input`.
/// Returns `Some(suggestion)` if a known value is within edit distance 3.
fn suggest_from<'a>(input: &str, known: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(&str, usize)> = None;
    for &candidate in known {
        let dist = edit_distance(input, candidate);
        if dist <= 3 && best.as_ref().is_none_or(|b| dist < b.1) {
            best = Some((candidate, dist));
        }
    }
    best.map(|(s, _)| s)
}

/// Validate an identifier against a known list, pushing an error with
/// an optional did-you-mean suggestion.
fn validate_ident_against(
    input: &str,
    kind: &str,
    known: &[&str],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    if known.contains(&input) {
        return;
    }
    let known_list = known.join(", ");
    let msg = format!("unknown {kind} \"{input}\", expected one of: {known_list}");
    if let Some(suggestion) = suggest_from(input, known) {
        errors.push(DslError::validation_with_suggestion(
            line,
            col,
            msg,
            format!("did you mean \"{suggestion}\"?"),
        ));
    } else {
        errors.push(DslError::validation(line, col, msg));
    }
}

/// Validate a Value that can be a named identifier or a numeric value.
/// `number_check_fn` returns `Some(error_message)` if the number is invalid,
/// or `None` if it's acceptable.
fn validate_named_or_numeric(
    val: &Value,
    kind: &str,
    known_names: &[&str],
    number_check_fn: impl FnOnce(f64, &str) -> Option<String>,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    match val {
        Value::Number(n, raw) => {
            if let Some(msg) = number_check_fn(*n, raw) {
                errors.push(DslError::validation(line, col, msg));
            }
        }
        Value::Ident(s) => {
            validate_ident_against(s, &format!("{kind} value"), known_names, line, col, errors);
        }
        _ => {
            errors.push(DslError::validation(
                line,
                col,
                format!(
                    "invalid {kind} value, \
                     expected a number or one of: {}",
                    known_names.join(", ")
                ),
            ));
        }
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, val) in dp[0].iter_mut().enumerate() {
        *val = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[a.len()][b.len()]
}

fn validate_hw_settings(
    settings: &[(String, Value)],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    let mut has_hw = false;
    let mut has_hw_fallback = false;

    for (key, val) in settings {
        if key == "hw" {
            has_hw = true;
            validate_ident_setting(val, "hw", VALID_HW_VALUES, line, col, errors);
        } else if key == "hw_fallback" {
            has_hw_fallback = true;
        }
    }

    if has_hw_fallback && !has_hw {
        errors.push(DslError::validation(
            line,
            col,
            "hw_fallback has no effect without hw".to_string(),
        ));
    }
}

const VALID_HDR_MODES: &[&str] = &["preserve", "tonemap"];
const VALID_TONEMAP_ALGORITHMS: &[&str] = &["bt2390", "hable", "mobius", "reinhard", "clip"];
const VALID_HDR_COLOR_METADATA: &[&str] = &["copy"];
const VALID_DOLBY_VISION_MODES: &[&str] = &["copy_rpu"];
const VALID_TUNE_VALUES: &[&str] = &[
    "film",
    "animation",
    "grain",
    "stillimage",
    "fastdecode",
    "zerolatency",
    "psnr",
    "ssim",
];
const VALID_SCALE_ALGORITHMS: &[&str] = &[
    "lanczos", "bicubic", "bilinear", "neighbor", "area", "spline", "sinc",
];

const VIDEO_ONLY_KEYS: &[&str] = &[
    "hdr_mode",
    "preserve_hdr",
    "tonemap",
    "hdr_color_metadata",
    "dolby_vision",
    "tune",
    "scale_algorithm",
    "max_resolution",
    "target_vmaf",
    "max_bitrate",
    "min_bitrate",
    "sample_strategy",
    "fallback",
    "crop",
    "crop_sample_duration",
    "crop_sample_count",
    "crop_threshold",
    "crop_minimum",
    "crop_preserve_bottom_pixels",
    "crop_aspect_lock",
];

fn reject_video_only_keys(
    settings: &[(String, Value)],
    target: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    for (key, _) in settings {
        if VIDEO_ONLY_KEYS.contains(&key.as_str()) || key.starts_with("target_vmaf_when ") {
            errors.push(DslError::validation(
                line,
                col,
                format!(
                    "{key} is only valid for video transcodes, \
                     not {target}"
                ),
            ));
        }
    }
}

fn validate_video_transcode_settings(
    settings: &[(String, Value)],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    let crop_enabled = settings.iter().any(|(key, val)| {
        key == "crop" && matches!(val, Value::Ident(mode) | Value::String(mode) if mode == "auto")
    });
    for (key, val) in settings {
        match key.as_str() {
            "hdr_mode" => {
                validate_ident_setting(val, "hdr_mode", VALID_HDR_MODES, line, col, errors);
            }
            "preserve_hdr" => validate_bool_setting(val, "preserve_hdr", line, col, errors),
            "tonemap" => {
                validate_ident_setting(val, "tonemap", VALID_TONEMAP_ALGORITHMS, line, col, errors);
            }
            "hdr_color_metadata" => {
                validate_ident_setting(
                    val,
                    "hdr_color_metadata",
                    VALID_HDR_COLOR_METADATA,
                    line,
                    col,
                    errors,
                );
            }
            "dolby_vision" => {
                validate_ident_setting(
                    val,
                    "dolby_vision",
                    VALID_DOLBY_VISION_MODES,
                    line,
                    col,
                    errors,
                );
            }
            "tune" => {
                validate_ident_setting(val, "tune", VALID_TUNE_VALUES, line, col, errors);
            }
            "scale_algorithm" => {
                validate_ident_setting(
                    val,
                    "scale_algorithm",
                    VALID_SCALE_ALGORITHMS,
                    line,
                    col,
                    errors,
                );
            }
            "max_resolution" => {
                let res_str = match val {
                    Value::Ident(s) | Value::String(s) => Some(s.as_str()),
                    Value::Number(_, raw) => Some(raw.as_str()),
                    _ => None,
                };
                match res_str {
                    Some(s) => {
                        let valid_resolutions =
                            ["480p", "720p", "1080p", "1440p", "2160p", "4k", "8k"];
                        if !valid_resolutions.contains(&s) {
                            errors.push(DslError::validation(
                                line,
                                col,
                                format!(
                                    "invalid max_resolution \"{s}\", \
                                     expected one of: {}",
                                    valid_resolutions.join(", ")
                                ),
                            ));
                        }
                    }
                    None => {
                        errors.push(DslError::validation(
                            line,
                            col,
                            "max_resolution must be a resolution value \
                             (e.g. 1080p, 720p, 4k, 2160p)"
                                .to_string(),
                        ));
                    }
                }
            }
            "crop" => validate_crop_mode(val, line, col, errors),
            "crop_sample_duration" | "crop_sample_count" => {
                validate_crop_enabled(crop_enabled, key, line, col, errors);
                validate_crop_positive_integer(val, key, line, col, errors);
            }
            "crop_threshold" => {
                validate_crop_enabled(crop_enabled, key, line, col, errors);
                validate_crop_integer_bounds(val, key, 0, 255, line, col, errors);
            }
            "crop_minimum" | "crop_preserve_bottom_pixels" => {
                validate_crop_enabled(crop_enabled, key, line, col, errors);
                validate_crop_integer_bounds(val, key, 0, u32::MAX, line, col, errors);
            }
            "crop_aspect_lock" => {
                validate_crop_enabled(crop_enabled, key, line, col, errors);
                validate_crop_aspect_lock(val, line, col, errors);
            }
            _ => {}
        }
    }
}

fn validate_crop_enabled(
    crop_enabled: bool,
    key: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    if !crop_enabled {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} has no effect without crop: auto"),
        ));
    }
}

fn validate_crop_mode(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    match val {
        Value::Ident(mode) | Value::String(mode) if mode == "auto" => {}
        _ => errors.push(DslError::validation(
            line,
            col,
            "crop must be auto".to_string(),
        )),
    }
}

fn validate_crop_positive_integer(
    val: &Value,
    key: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    let Some(value) = numeric_u32(val) else {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} must be a positive integer"),
        ));
        return;
    };
    if value == 0 {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} must be greater than 0"),
        ));
    }
}

fn validate_crop_integer_bounds(
    val: &Value,
    key: &str,
    min: u32,
    max: u32,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    let Some(value) = numeric_u32(val) else {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} must be an integer"),
        ));
        return;
    };
    if value < min || value > max {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} must be from {min} to {max}"),
        ));
    }
}

fn validate_crop_aspect_lock(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    let Value::List(items) = val else {
        errors.push(DslError::validation(
            line,
            col,
            "crop_aspect_lock must be a list of ratios like [16/9, 4/3]".to_string(),
        ));
        return;
    };
    for item in items {
        let Some(ratio) = value_string(item) else {
            errors.push(DslError::validation(
                line,
                col,
                "crop_aspect_lock entries must be ratios like 16/9".to_string(),
            ));
            continue;
        };
        if !is_crop_ratio(ratio) {
            errors.push(DslError::validation(
                line,
                col,
                format!("invalid crop_aspect_lock ratio \"{ratio}\", expected WIDTH/HEIGHT"),
            ));
        }
    }
}

fn value_string(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) | Value::Ident(s) | Value::Number(_, s) => Some(s.as_str()),
        _ => None,
    }
}

fn is_crop_ratio(value: &str) -> bool {
    let Some((width, height)) = value.split_once('/') else {
        return false;
    };
    let Ok(width) = width.parse::<u32>() else {
        return false;
    };
    let Ok(height) = height.parse::<u32>() else {
        return false;
    };
    width > 0 && height > 0
}

fn validate_vmaf_transcode_settings(
    settings: &[(String, Value)],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    let mut min_bitrate = None;
    let mut max_bitrate = None;

    for (key, val) in settings {
        match key.as_str() {
            "target_vmaf" => validate_vmaf_target(val, "target_vmaf", line, col, errors, warnings),
            "min_bitrate" => min_bitrate = validate_bitrate_value(val, key, line, col, errors),
            "max_bitrate" => max_bitrate = validate_bitrate_value(val, key, line, col, errors),
            "sample_strategy" => validate_sample_strategy(val, line, col, errors),
            "fallback" => validate_transcode_fallback(val, line, col, errors),
            _ if key.starts_with("target_vmaf_when ") => {
                validate_vmaf_override(key, val, line, col, errors, warnings);
            }
            _ => {}
        }
    }

    if let (Some(min), Some(max)) = (min_bitrate, max_bitrate) {
        if min > max {
            errors.push(DslError::validation(
                line,
                col,
                "min_bitrate must be less than or equal to max_bitrate".to_string(),
            ));
        }
    }
}

fn validate_vmaf_target(
    val: &Value,
    key: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    let Some(target) = numeric_u32(val) else {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} must be an integer from 60 to 100"),
        ));
        return;
    };
    if !(60..=100).contains(&target) {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} must be from 60 to 100"),
        ));
    } else if !(80..=99).contains(&target) {
        warnings.push(DslWarning::new(
            line,
            col,
            format!("{key} value {target} is outside the typical 80-99 range"),
            None,
        ));
    }
}

fn validate_vmaf_override(
    key: &str,
    val: &Value,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    let Some(path) = key.strip_prefix("target_vmaf_when ") else {
        return;
    };
    if !path.starts_with("content.") {
        errors.push(DslError::validation(
            line,
            col,
            "target_vmaf_when requires a content.<type> path".to_string(),
        ));
    }
    validate_vmaf_target(val, "target_vmaf_when", line, col, errors, warnings);
}

fn validate_bitrate_value(
    val: &Value,
    key: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) -> Option<u64> {
    let raw = match val {
        Value::String(s) | Value::Ident(s) | Value::Number(_, s) => s.as_str(),
        _ => {
            errors.push(DslError::validation(
                line,
                col,
                format!("{key} must be a bitrate string such as \"8M\" or \"500k\""),
            ));
            return None;
        }
    };
    parse_bitrate(raw).or_else(|| {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} has invalid bitrate \"{raw}\""),
        ));
        None
    })
}

fn parse_bitrate(raw: &str) -> Option<u64> {
    let split_at = raw
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(raw.len(), |(idx, _)| idx);
    let (digits, suffix) = raw.split_at(split_at);
    let value = digits.parse::<u64>().ok()?;
    let multiplier = match suffix {
        "k" | "K" => 1_000,
        "m" | "M" => 1_000_000,
        "g" | "G" => 1_000_000_000,
        "" => 1,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

fn validate_sample_strategy(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    match val {
        Value::Ident(name) if name == "full" => {}
        Value::Call { name, args } if name == "scenes" || name == "uniform" => {
            if numeric_arg(args, "count").is_none() {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!("{name} sample_strategy requires integer count"),
                ));
            }
            if string_arg(args, "duration").is_none() {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!("{name} sample_strategy requires duration"),
                ));
            }
        }
        _ => errors.push(DslError::validation(
            line,
            col,
            "sample_strategy must be full, scenes(count: N, duration: Ts), or uniform(count: N, duration: Ts)"
                .to_string(),
        )),
    }
}

fn validate_transcode_fallback(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    let Value::Object(items) = val else {
        errors.push(DslError::validation(
            line,
            col,
            "fallback must be a nested block".to_string(),
        ));
        return;
    };
    match numeric_arg(items, "crf") {
        Some(crf) if crf <= 51 => {}
        Some(_) => errors.push(DslError::validation(
            line,
            col,
            "fallback.crf must be from 0 to 51".to_string(),
        )),
        None => errors.push(DslError::validation(
            line,
            col,
            "fallback requires crf".to_string(),
        )),
    }
    if string_arg(items, "preset").is_none() {
        errors.push(DslError::validation(
            line,
            col,
            "fallback requires preset".to_string(),
        ));
    }
}

fn numeric_arg(items: &[(String, Value)], key: &str) -> Option<u32> {
    items
        .iter()
        .find(|(item_key, _)| item_key == key)
        .and_then(|(_, value)| numeric_u32(value))
}

fn string_arg<'a>(items: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    items
        .iter()
        .find(|(item_key, _)| item_key == key)
        .and_then(|(_, value)| match value {
            Value::String(s) | Value::Ident(s) | Value::Number(_, s) => Some(s.as_str()),
            _ => None,
        })
}

fn numeric_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(n, _) if *n >= 0.0 && *n <= f64::from(u32::MAX) && n.fract() == 0.0 =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(*n as u32)
        }
        _ => None,
    }
}

fn validate_ident_setting(
    val: &Value,
    key: &str,
    valid: &[&str],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    let name = match val {
        Value::Ident(s) | Value::String(s) => Some(s.as_str()),
        _ => None,
    };
    if let Some(name) = name {
        let kind = format!("{key} value");
        validate_ident_against(name, &kind, valid, line, col, errors);
    } else {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "{key} must be a string or identifier, got {}",
                match val {
                    Value::Number(_, _) => "number",
                    Value::Bool(_) => "boolean",
                    Value::List(_) => "list",
                    Value::Object(_) => "object",
                    Value::Call { .. } => "call",
                    _ => "unknown",
                }
            ),
        ));
    }
}

fn validate_bool_setting(
    val: &Value,
    key: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    if !matches!(val, Value::Bool(_)) {
        errors.push(DslError::validation(
            line,
            col,
            format!("{key} requires a boolean value"),
        ));
    }
}

const KNOWN_CHANNEL_NAMES: &[&str] = &["mono", "stereo", "5.1", "surround", "7.1", "preserve"];

const KNOWN_POSITION_NAMES: &[&str] = &["after_source", "last", "beginning"];

fn validate_synth_channels(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    validate_named_or_numeric(
        val,
        "channels",
        KNOWN_CHANNEL_NAMES,
        |n, raw| {
            if raw == "5.1" || raw == "7.1" {
                return None;
            }
            if has_number_suffix(raw) {
                return Some(format!(
                    "invalid channels value \"{raw}\", \
                     suffixed numbers are not valid here; \
                     expected a positive integer or one of: {}",
                    KNOWN_CHANNEL_NAMES.join(", ")
                ));
            }
            if n.fract() != 0.0 || n <= 0.0 || n > f64::from(u32::MAX) {
                return Some(format!(
                    "invalid channels value \"{raw}\", \
                     expected a positive integer or one of: {}",
                    KNOWN_CHANNEL_NAMES.join(", ")
                ));
            }
            None
        },
        line,
        col,
        errors,
    );
}

fn validate_synth_position(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    validate_named_or_numeric(
        val,
        "position",
        KNOWN_POSITION_NAMES,
        |n, raw| {
            if has_number_suffix(raw) {
                return Some(format!(
                    "invalid position value \"{raw}\", \
                     suffixed numbers are not valid here; \
                     expected a non-negative integer or one of: {}",
                    KNOWN_POSITION_NAMES.join(", ")
                ));
            }
            if n.fract() != 0.0 || n < 0.0 || n > f64::from(u32::MAX) {
                return Some(format!(
                    "invalid position value \"{raw}\", \
                     expected a non-negative integer or one of: {}",
                    KNOWN_POSITION_NAMES.join(", ")
                ));
            }
            None
        },
        line,
        col,
        errors,
    );
}

const KNOWN_ACTIONS_KEYS: &[&str] = &["clear_all_default", "clear_all_forced", "clear_all_titles"];

fn validate_actions_settings(
    settings: &[(String, Value)],
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    for (key, val) in settings {
        if !KNOWN_ACTIONS_KEYS.contains(&key.as_str()) {
            validate_ident_against(
                key,
                "actions setting",
                KNOWN_ACTIONS_KEYS,
                line,
                col,
                errors,
            );
        } else if !matches!(val, Value::Bool(_)) {
            errors.push(DslError::validation(
                line,
                col,
                format!("actions setting \"{key}\" requires a boolean value"),
            ));
        }
    }
}

fn validate_when(
    when: &WhenNode,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    let line = when.span.line;
    let col = when.span.col;
    validate_condition(&when.condition, line, col, phase_names, errors, warnings);
    for action in &when.then_actions {
        validate_action(action, line, col, phase_names, errors, warnings);
    }
    for action in &when.else_actions {
        validate_action(action, line, col, phase_names, errors, warnings);
    }
}

fn validate_action(
    action: &ActionNode,
    line: usize,
    col: usize,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    match action {
        ActionNode::Keep { target, filter } | ActionNode::Remove { target, filter } => {
            validate_track_target(target, line, col, errors);
            if let Some(f) = filter {
                validate_filter(f, line, col, phase_names, errors, warnings);
            }
        }
        ActionNode::SetDefault(track_ref) | ActionNode::SetForced(track_ref) => {
            validate_track_target(&track_ref.target, line, col, errors);
            if let Some(f) = &track_ref.filter {
                validate_filter(f, line, col, phase_names, errors, warnings);
            }
        }
        ActionNode::SetLanguage(track_ref, val) => {
            validate_track_target(&track_ref.target, line, col, errors);
            if let Some(f) = &track_ref.filter {
                validate_filter(f, line, col, phase_names, errors, warnings);
            }
            match val {
                ValueOrField::Value(Value::String(s)) => {
                    if !language::is_valid_language(s) {
                        errors.push(DslError::validation(
                            line,
                            col,
                            format!("unknown language code \"{s}\" in set_language action"),
                        ));
                    }
                }
                ValueOrField::Field(path) => {
                    validate_field_path(path, line, col, phase_names, errors, warnings);
                }
                ValueOrField::Value(_) => {}
            }
        }
        ActionNode::SetTag(_, val) => {
            if let ValueOrField::Field(path) = val {
                validate_field_path(path, line, col, phase_names, errors, warnings);
            }
        }
        ActionNode::Skip(_) | ActionNode::Warn(_) | ActionNode::Fail(_) => {}
    }
}

fn validate_condition(
    cond: &ConditionNode,
    line: usize,
    col: usize,
    phase_names: &HashSet<&str>,
    errors: &mut Vec<DslError>,
    warnings: &mut Vec<DslWarning>,
) {
    match cond {
        ConditionNode::Exists(query) | ConditionNode::Count(query, _, _) => {
            validate_track_target(&query.target, line, col, errors);
            if let Some(f) = &query.filter {
                validate_filter(f, line, col, phase_names, errors, warnings);
            }
        }
        ConditionNode::FieldCompare(path, _, value) => {
            validate_field_path(path, line, col, phase_names, errors, warnings);
            validate_value(value, line, col, errors);
        }
        ConditionNode::FieldExists(path) => {
            validate_field_path(path, line, col, phase_names, errors, warnings);
        }
        ConditionNode::And(items) | ConditionNode::Or(items) => {
            for item in items {
                validate_condition(item, line, col, phase_names, errors, warnings);
            }
        }
        ConditionNode::Not(inner) => {
            validate_condition(inner, line, col, phase_names, errors, warnings);
        }
        ConditionNode::AudioIsMultiLanguage
        | ConditionNode::IsDubbed
        | ConditionNode::IsOriginal => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_policy;

    #[test]
    fn test_valid_policy_passes() {
        let input = r#"policy "test" {
            config {
                languages audio: [eng, und]
                languages subtitle: [eng]
                on_error: continue
            }
            phase init {
                container mkv
            }
            phase norm {
                depends_on: [init]
                keep audio where lang in [eng, jpn]
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_valid_vmaf_transcode_settings_pass() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    target_vmaf: 93
                    min_bitrate: "2M"
                    max_bitrate: "8M"
                    sample_strategy: uniform(count: 8, duration: 5s)
                    fallback {
                        crf: 24
                        preset: "medium"
                    }
                    target_vmaf_when content.animation: 88
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_invalid_vmaf_target_rejected() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    target_vmaf: 101
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();

        assert!(err
            .errors
            .iter()
            .any(|e| format!("{e}").contains("target_vmaf must be from 60 to 100")));
    }

    #[test]
    fn test_invalid_vmaf_bitrate_order_rejected() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    min_bitrate: "8M"
                    max_bitrate: "2M"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();

        assert!(err
            .errors
            .iter()
            .any(|e| format!("{e}").contains("min_bitrate must be less")));
    }

    #[test]
    fn test_invalid_vmaf_fallback_crf_rejected() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    fallback {
                        crf: 52
                        preset: medium
                    }
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();

        assert!(err
            .errors
            .iter()
            .any(|e| format!("{e}").contains("fallback.crf must be from 0 to 51")));
    }

    #[test]
    fn test_unknown_codec() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to h256 {
                    crf: 20
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let msg = format!("{}", err.errors[0]);
        assert!(msg.contains("unknown codec \"h256\""));
        // Should get did-you-mean suggestion
        match &err.errors[0] {
            DslError::Validation { suggestion, .. } => {
                assert!(suggestion.is_some());
                let s = suggestion.as_ref().unwrap();
                assert!(s.contains("h264") || s.contains("hevc"));
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn test_circular_dependency() {
        let input = r#"policy "test" {
            phase a {
                depends_on: [b]
                container mkv
            }
            phase b {
                depends_on: [a]
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let msgs: Vec<String> = err.errors.iter().map(|e| format!("{e}")).collect();
        assert!(msgs.iter().any(|m| m.contains("circular dependency")));
    }

    #[test]
    fn test_unknown_phase_reference() {
        let input = r#"policy "test" {
            phase init {
                depends_on: [nonexistent]
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(format!("{}", err.errors[0]).contains("unknown phase \"nonexistent\""));
    }

    #[test]
    fn test_self_dependency() {
        let input = r#"policy "test" {
            phase init {
                depends_on: [init]
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(format!("{}", err.errors[0]).contains("depends on itself"));
    }

    #[test]
    fn test_invalid_on_error() {
        let input = r#"policy "test" {
            config {
                on_error: explode
            }
            phase init { container mkv }
        }"#;
        let err = parse_policy(input).unwrap_err();
        assert!(format!("{err}").contains("invalid on_error"));
    }

    #[test]
    fn test_unknown_container() {
        let input = r#"policy "test" {
            phase init {
                container zzz
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let msg = format!("{}", err.errors[0]);
        assert!(msg.contains("unknown container 'zzz'"), "got: {msg}");
        assert!(msg.contains("mkv"), "should list known extensions: {msg}");
    }

    #[test]
    fn test_conflicting_keep_remove_unfiltered() {
        let input = r#"policy "test" {
            phase norm {
                keep audio
                remove audio
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(format!("{}", err.errors[0]).contains("conflicting keep and remove"));
    }

    #[test]
    fn test_production_policy_valid() {
        let input = include_str!("../tests/fixtures/production-normalize.voom");
        let ast = parse_policy(input).unwrap();
        // This policy uses valid codecs, languages, phases, etc.
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_conflicting_keep_remove_synonyms_unfiltered() {
        // "keep subtitles" + "remove subtitle" should conflict when both unfiltered
        let input = r#"policy "test" {
            phase norm {
                keep subtitles
                remove subtitle
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("conflicting keep and remove")),
            "expected conflict error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_exists_track_target_valid() {
        let input = r#"policy "test" {
            phase validate {
                when exists(track where codec == hevc) {
                    warn "has hevc track"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let result = validate(&ast);
        assert!(
            result.is_ok(),
            "validation errors: {:?}",
            result.unwrap_err().errors
        );
    }

    #[test]
    fn test_number_suffix_valid() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    bitrate: 192k
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_number_suffix_invalid() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    bitrate: 192x
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown number suffix")),
            "expected suffix error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_operation_level_line_numbers() {
        // Errors from operations should report the operation's line, not the phase's
        let input = r#"policy "test" {
            phase norm {
                container zzz
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        match &err.errors[0] {
            DslError::Validation { line, .. } => {
                // "container zzz" is on line 3, not the phase start (line 2)
                assert_eq!(
                    *line, 3,
                    "error should report operation line, not phase line"
                );
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn test_clear_tags_valid() {
        let input = r#"policy "test" {
            phase clean {
                clear_tags
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_set_tag_valid() {
        let input = r#"policy "test" {
            phase clean {
                set_tag "title" "My Movie"
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_delete_tag_valid() {
        let input = r#"policy "test" {
            phase clean {
                delete_tag "encoder"
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_invalid_hw_value() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    hw: nvencc
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let msg = format!("{}", err.errors[0]);
        assert!(
            msg.contains("unknown hw value \"nvencc\""),
            "expected hw error, got: {msg}"
        );
        match &err.errors[0] {
            DslError::Validation { suggestion, .. } => {
                assert!(suggestion.is_some(), "expected did-you-mean suggestion");
                let s = suggestion.as_ref().unwrap();
                assert!(s.contains("nvenc"), "expected nvenc suggestion, got: {s}");
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn test_hw_fallback_without_hw() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    hw_fallback: true
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("hw_fallback has no effect without hw")),
            "expected hw_fallback warning, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_unknown_transcode_key() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crf: 20
                    foobar: 42
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown transcode setting \"foobar\"")),
            "expected unknown key error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_unknown_transcode_key_with_suggestion() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    prset: medium
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let key_err = err
            .errors
            .iter()
            .find(|e| format!("{e}").contains("unknown transcode setting"))
            .expect("expected unknown key error");
        match key_err {
            DslError::Validation { suggestion, .. } => {
                assert!(suggestion.is_some(), "expected did-you-mean suggestion");
                let s = suggestion.as_ref().unwrap();
                assert!(s.contains("preset"), "expected preset suggestion, got: {s}");
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn test_transcode_crop_settings_validate() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crop: auto
                    crop_sample_duration: 30
                    crop_sample_count: 4
                    crop_threshold: 18
                    crop_minimum: 6
                    crop_preserve_bottom_pixels: 40
                    crop_aspect_lock: ["16/9", "4/3"]
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        validate(&ast).unwrap();
    }

    #[test]
    fn test_transcode_crop_rejected_for_audio() {
        let input = r#"policy "test" {
            phase tc {
                transcode audio to aac {
                    crop: auto
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("crop is only valid for video transcodes")),
            "expected video-only crop error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_transcode_crop_invalid_aspect_lock() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crop: auto
                    crop_aspect_lock: ["16x9"]
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("invalid crop_aspect_lock ratio")),
            "expected aspect lock error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_transcode_crop_tuning_requires_crop_auto() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crop_threshold: 18
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("crop_threshold has no effect without crop: auto")),
            "expected crop tuning no-op error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_codec_track_type_mismatch_video_to_audio_codec() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to aac {
                    crf: 20
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}")
                    .contains("codec \"aac\" is an audio codec but target is video")),
            "expected codec-track mismatch error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_codec_track_type_mismatch_audio_to_video_codec() {
        let input = r#"policy "test" {
            phase tc {
                transcode audio to hevc {
                    crf: 20
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}")
                    .contains("codec \"hevc\" is a video codec but target is audio")),
            "expected codec-track mismatch error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_valid_transcode_all_known_keys() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crf: 20
                    preset: medium
                    hw: auto
                    hw_fallback: true
                    max_resolution: 1080p
                    scale_algorithm: lanczos
                    hdr_mode: preserve
                    tune: film
                }
                transcode audio to aac {
                    preserve: [truehd, dts_hd, flac]
                    bitrate: 192k
                    channels: stereo
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(
            validate(&ast).is_ok(),
            "all known transcode keys should be valid"
        );
    }

    #[test]
    fn test_invalid_regex_in_title_matches() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where title matches "[invalid"
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("invalid regex pattern")),
            "expected regex validation error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_filtered_keep_remove_no_conflict() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang in [eng]
                remove audio where commentary
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(
            validate(&ast).is_ok(),
            "filtered keep+remove on same target should not conflict"
        );
    }

    #[test]
    fn test_two_independent_cycles_both_reported() {
        let input = r#"policy "test" {
            phase a {
                depends_on: [b]
                container mkv
            }
            phase b {
                depends_on: [a]
                container mkv
            }
            phase c {
                depends_on: [d]
                container mkv
            }
            phase d {
                depends_on: [c]
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let cycle_errors: Vec<_> = err
            .errors
            .iter()
            .filter(|e| format!("{e}").contains("circular dependency"))
            .collect();
        assert!(
            cycle_errors.len() >= 2,
            "expected at least 2 cycle errors, got {}: {:?}",
            cycle_errors.len(),
            cycle_errors
        );
    }

    #[test]
    fn test_valid_hw_values() {
        for hw in &["auto", "nvenc", "qsv", "vaapi", "videotoolbox", "none"] {
            let input = format!(
                r#"policy "test" {{
                    phase tc {{
                        transcode video to hevc {{
                            hw: {hw}
                        }}
                    }}
                }}"#
            );
            let ast = parse_policy(&input).unwrap();
            assert!(validate(&ast).is_ok(), "hw value \"{hw}\" should be valid");
        }
    }

    #[test]
    fn test_video_only_setting_on_audio_transcode() {
        let input = r#"policy "test" {
            phase tc {
                transcode audio to aac {
                    hdr_mode: preserve
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors.iter().any(|e| {
                let msg = format!("{e}");
                msg.contains("hdr_mode") && msg.contains("only valid for video")
            }),
            "expected video-only key error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_video_only_settings_all_rejected_on_audio() {
        for key in &["hdr_mode", "tune", "scale_algorithm", "max_resolution"] {
            let value = if *key == "max_resolution" {
                "1080p"
            } else if *key == "tune" {
                "film"
            } else if *key == "scale_algorithm" {
                "lanczos"
            } else {
                "preserve"
            };
            let input = format!(
                r#"policy "test" {{
                    phase tc {{
                        transcode audio to aac {{
                            {key}: {value}
                        }}
                    }}
                }}"#
            );
            let ast = parse_policy(&input).unwrap();
            let err = validate(&ast).unwrap_err();
            assert!(
                err.errors
                    .iter()
                    .any(|e| format!("{e}").contains("only valid for video")),
                "{key} on audio should be rejected, got: {:?}",
                err.errors
            );
        }
    }

    #[test]
    fn test_valid_max_resolution_values() {
        for res in &["480p", "720p", "1080p", "1440p", "2160p", "4k", "8k"] {
            let input = format!(
                r#"policy "test" {{
                    phase tc {{
                        transcode video to hevc {{
                            max_resolution: {res}
                        }}
                    }}
                }}"#
            );
            let ast = parse_policy(&input).unwrap();
            assert!(
                validate(&ast).is_ok(),
                "max_resolution \"{res}\" should be valid"
            );
        }
    }

    #[test]
    fn test_invalid_max_resolution_value() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    max_resolution: 999p
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("invalid max_resolution")),
            "expected invalid max_resolution error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_lang_field_valid_root() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.radarr.original_language
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_lang_field_invalid_root() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == unknown.field.path
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown field root")),
            "expected unknown field root error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_codec_field_valid() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where codec != plugin.detector.codec
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_zxx_language_valid() {
        let input = r#"policy "test" {
            phase norm {
                remove audio where lang == zxx
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_mul_language_valid() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == mul
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_skip_when_unknown_field_root() {
        let input = r#"policy "test" {
            phase norm {
                skip when unknown_root.field exists
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown field root")),
            "expected unknown field root error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_skip_when_phase_output_outcome_valid() {
        let input = r#"policy "test" {
            phase verify {
                verify quick
            }
            phase backup {
                depends_on: [verify]
                skip when verify.outcome != "ok"
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let result = validate(&ast);
        assert!(
            result.is_ok(),
            "verify.outcome should be a valid phase output field: {:?}",
            result.unwrap_err().errors
        );
    }

    #[test]
    fn test_skip_when_phase_output_unknown_field() {
        let input = r#"policy "test" {
            phase verify {
                verify quick
            }
            phase backup {
                depends_on: [verify]
                skip when verify.bogus_field == "ok"
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown phase output field")),
            "expected unknown phase output field error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_skip_when_phase_output_nested_field_rejected() {
        // Phase outputs are flat; nested access like verify.outcome.code is invalid.
        let input = r#"policy "test" {
            phase verify {
                verify quick
            }
            phase backup {
                depends_on: [verify]
                skip when verify.outcome.code == "ok"
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("does not support nested fields")),
            "expected nested-field rejection, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_skip_when_valid_field_path() {
        let input = r#"policy "test" {
            phase norm {
                skip when plugin.radarr.year > 2020
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(
            validate(&ast).is_ok(),
            "plugin.radarr.year should be a valid field path: {:?}",
            validate(&ast).unwrap_err().errors
        );
    }

    #[test]
    fn test_plugin_name_warning_typo() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.radrr.original_language
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let (warnings, result) = validate_with_warnings(&ast);
        assert!(result.is_ok(), "typo in plugin name should not be an error");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0]
            .message
            .contains("unknown plugin name \"radrr\""));
        assert!(
            warnings[0].suggestion.as_ref().unwrap().contains("radarr"),
            "should suggest radarr, got: {:?}",
            warnings[0].suggestion
        );
    }

    #[test]
    fn test_plugin_name_no_warning() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.radarr.original_language
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let (warnings, result) = validate_with_warnings(&ast);
        assert!(result.is_ok());
        assert!(
            warnings.is_empty(),
            "known plugin name should produce no warnings, got: {warnings:?}"
        );
    }

    #[test]
    fn test_plugin_name_no_warning_for_wasm_plugins() {
        // Plugin names far from all known plugins are assumed to be WASM plugins
        // and should not trigger a warning.
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.custom_wasm.original_language
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let (warnings, result) = validate_with_warnings(&ast);
        assert!(result.is_ok(), "unknown plugin name should not be an error");
        assert!(
            warnings.is_empty(),
            "WASM plugin names should not produce warnings, got: {warnings:?}"
        );
    }

    #[test]
    fn test_plugin_name_warning_for_typo() {
        // Plugin names close to a known plugin should warn (likely typo).
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.sonar.original_language
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let (warnings, result) = validate_with_warnings(&ast);
        assert!(result.is_ok());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0]
            .message
            .contains("unknown plugin name \"sonar\""));
        assert!(
            warnings[0].suggestion.is_some(),
            "close name should have suggestion"
        );
    }

    // --- Fix 1: FieldCompare RHS validation ---

    #[test]
    fn test_field_compare_valid_suffix_passes() {
        let input = r#"policy "test" {
            phase norm {
                when file.size > 10G {
                    warn "large file"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_field_compare_bare_number_passes() {
        let input = r#"policy "test" {
            phase norm {
                when file.size > 10 {
                    warn "large file"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_field_compare_invalid_suffix_errors() {
        let input = r#"policy "test" {
            phase norm {
                when file.size > 10z {
                    warn "large file"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown number suffix")),
            "expected suffix error, got: {:?}",
            err.errors
        );
    }

    // --- Fix 2: Actions block key/value validation ---

    #[test]
    fn test_actions_valid_passes() {
        let input = r#"policy "test" {
            phase norm {
                audio actions {
                    clear_all_default: true
                    clear_all_forced: false
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_actions_unknown_key_errors() {
        let input = r#"policy "test" {
            phase norm {
                audio actions {
                    bogus_key: true
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown actions setting \"bogus_key\"")),
            "expected unknown key error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_actions_typo_suggests() {
        let input = r#"policy "test" {
            phase norm {
                audio actions {
                    clear_all_defualt: true
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        let key_err = err
            .errors
            .iter()
            .find(|e| format!("{e}").contains("unknown actions setting"))
            .expect("expected unknown key error");
        match key_err {
            DslError::Validation { suggestion, .. } => {
                assert!(suggestion.is_some(), "expected did-you-mean suggestion");
                let s = suggestion.as_ref().unwrap();
                assert!(
                    s.contains("clear_all_default"),
                    "expected clear_all_default suggestion, got: {s}"
                );
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn test_actions_non_bool_value_errors() {
        let input = r#"policy "test" {
            phase norm {
                audio actions {
                    clear_all_default: 42
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("requires a boolean value")),
            "expected boolean type error, got: {:?}",
            err.errors
        );
    }

    // --- Fix 3: Synthesize channels and position validation ---

    #[test]
    fn test_synthesize_channels_named_passes() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    channels: stereo
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_synthesize_channels_numeric_passes() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    channels: 2
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_synthesize_channels_5_1_passes() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    channels: 5.1
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_synthesize_channels_unknown_errors() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    channels: quadraphonic
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown channels value \"quadraphonic\"")),
            "expected unknown channels error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_synthesize_position_named_passes() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    position: after_source
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_synthesize_position_numeric_passes() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    position: 0
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert!(validate(&ast).is_ok());
    }

    #[test]
    fn test_synthesize_position_fractional_errors() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    position: 2.5
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("invalid position value")),
            "expected position error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_synthesize_position_unknown_errors() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    position: middle
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("unknown position value \"middle\"")),
            "expected unknown position error, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_synthesize_channels_suffixed_number_errors() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    channels: 2k
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("suffixed numbers are not valid")),
            "expected suffix rejection, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_synthesize_position_suffixed_number_errors() {
        let input = r#"policy "test" {
            phase synth {
                synthesize "Test" {
                    codec: aac
                    position: 1k
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| format!("{e}").contains("suffixed numbers are not valid")),
            "expected suffix rejection, got: {:?}",
            err.errors
        );
    }

    // ---- validate_filter arm-deletion tests (issue #236, phase 2) ----
    // Each test invokes validate_filter with a focused FilterNode that the
    // original code rejects (or accepts) and the mutated code does the
    // opposite. For arm-deletion mutants, an invalid filter that originally
    // emits an error becomes silent under the mutant. For the `delete !`
    // mutant on line 735, both an invalid-language test (catches the deleted
    // arm + flipped predicate) and a valid-language test (catches the
    // flipped predicate alone) make the distinction explicit.

    use crate::ast::CompareOp;

    fn run_validate_filter(filter: FilterNode) -> (Vec<DslError>, Vec<DslWarning>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let phase_names: HashSet<&str> = HashSet::new();
        validate_filter(&filter, 1, 1, &phase_names, &mut errors, &mut warnings);
        (errors, warnings)
    }

    #[test]
    fn validate_filter_lang_in_invalid_emits_error() {
        let (errors, _) = run_validate_filter(FilterNode::LangIn(vec!["xxx".into()]));
        assert!(
            !errors.is_empty(),
            "expected an error for invalid language in LangIn, got none"
        );
    }

    #[test]
    fn validate_filter_lang_compare_invalid_emits_error() {
        let (errors, _) = run_validate_filter(FilterNode::LangCompare(CompareOp::Eq, "xxx".into()));
        assert!(
            !errors.is_empty(),
            "expected an error for invalid language in LangCompare, got none"
        );
    }

    #[test]
    fn validate_filter_lang_compare_valid_emits_no_error() {
        let (errors, _) = run_validate_filter(FilterNode::LangCompare(CompareOp::Eq, "eng".into()));
        assert!(
            errors.is_empty(),
            "expected no error for valid language in LangCompare, got: {errors:?}"
        );
    }

    #[test]
    fn validate_filter_codec_in_invalid_emits_error() {
        let (errors, _) = run_validate_filter(FilterNode::CodecIn(vec!["bogus_codec".into()]));
        assert!(
            !errors.is_empty(),
            "expected an error for invalid codec in CodecIn, got none"
        );
    }

    #[test]
    fn validate_filter_codec_compare_invalid_emits_error() {
        let (errors, _) = run_validate_filter(FilterNode::CodecCompare(
            CompareOp::Eq,
            "bogus_codec".into(),
        ));
        assert!(
            !errors.is_empty(),
            "expected an error for invalid codec in CodecCompare, got none"
        );
    }

    #[test]
    fn validate_filter_and_recurses_into_invalid_inner() {
        let (errors, _) =
            run_validate_filter(FilterNode::And(vec![FilterNode::LangIn(
                vec!["xxx".into()],
            )]));
        assert!(
            !errors.is_empty(),
            "expected And to recurse into LangIn and surface its error, got none"
        );
    }

    #[test]
    fn validate_filter_or_recurses_into_invalid_inner() {
        let (errors, _) =
            run_validate_filter(FilterNode::Or(vec![FilterNode::LangIn(vec!["xxx".into()])]));
        assert!(
            !errors.is_empty(),
            "expected Or to recurse into LangIn and surface its error, got none"
        );
    }

    #[test]
    fn validate_filter_not_recurses_into_invalid_inner() {
        let (errors, _) = run_validate_filter(FilterNode::Not(Box::new(FilterNode::LangIn(vec![
            "xxx".into(),
        ]))));
        assert!(
            !errors.is_empty(),
            "expected Not to recurse into LangIn and surface its error, got none"
        );
    }

    // EQUIVALENT MUTANT (issue #236, phase 2):
    // crates/voom-dsl/src/validator.rs:390:26: replace < with <= in check_tag_conflicts
    //
    // The compared values `i` and `clear_idx` both come from `enumerate()` on
    // `phase.operations` — they are unique sequential indices into the same
    // vec. The condition runs only when the spanned op at index `i` is a
    // SetTag; ClearTags has its own (different) index `clear_idx`. So `i`
    // cannot equal `clear_idx`, and `i < clear_idx` and `i <= clear_idx` are
    // always equivalent on every reachable input.
    //
    // Per #236 policy: documented inline rather than suppressed in
    // .cargo/mutants.toml so the analysis stays discoverable.

    // ---- check_tag_conflicts arm-deletion + boundary tests (issue #236, phase 2) ----

    fn phase_with_ops(name: &str, ops: Vec<OperationNode>) -> PhaseNode {
        use crate::ast::{Span, SpannedOperation};

        let operations = ops
            .into_iter()
            .enumerate()
            .map(|(i, node)| SpannedOperation {
                node,
                span: Span {
                    start: i,
                    end: i + 1,
                    line: i + 1,
                    col: 1,
                },
            })
            .collect();
        PhaseNode {
            name: name.into(),
            skip_when: None,
            depends_on: vec![],
            run_if: None,
            on_error: None,
            operations,
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
            },
        }
    }

    #[test]
    fn check_tag_conflicts_set_and_delete_same_tag_emits_error() {
        let phase = phase_with_ops(
            "p",
            vec![
                OperationNode::SetTag {
                    tag: "x".into(),
                    value: ValueOrField::Value(Value::String("dummy".into())),
                },
                OperationNode::DeleteTag("x".into()),
            ],
        );
        let mut errors = Vec::new();
        check_tag_conflicts(&phase, &mut errors);
        assert_eq!(
            errors.len(),
            1,
            "expected exactly one error, got {errors:?}"
        );
        let msg = format!("{}", errors[0]);
        assert!(
            msg.contains("both set and deleted"),
            "expected 'both set and deleted' in error, got: {msg}"
        );
    }

    #[test]
    fn check_tag_conflicts_set_before_clear_emits_error() {
        let phase = phase_with_ops(
            "p",
            vec![
                OperationNode::SetTag {
                    tag: "y".into(),
                    value: ValueOrField::Value(Value::String("dummy".into())),
                },
                OperationNode::ClearTags,
            ],
        );
        let mut errors = Vec::new();
        check_tag_conflicts(&phase, &mut errors);
        assert_eq!(
            errors.len(),
            1,
            "expected exactly one error, got {errors:?}"
        );
        let msg = format!("{}", errors[0]);
        assert!(
            msg.contains("appears before clear_tags"),
            "expected 'appears before clear_tags' in error, got: {msg}"
        );
    }
}
