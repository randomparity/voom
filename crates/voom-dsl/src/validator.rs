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
//! - Invalid `on_error` values
//! - Invalid container names
//! - Invalid `run_if` triggers

use std::collections::{HashMap, HashSet};

use voom_domain::utils::{codecs, codecs::CodecType, language};

use crate::ast::*;
use crate::errors::{DslError, ValidationErrors};

/// Validate a parsed AST for semantic correctness.
/// Returns `Ok(())` if valid, or `Err(ValidationErrors)` with all errors found.
pub fn validate(ast: &PolicyAst) -> std::result::Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    validate_config(ast, &mut errors);
    validate_phase_names(ast, &mut errors);
    validate_phase_references(ast, &mut errors);
    validate_cycle_detection(ast, &mut errors);
    validate_reachability(ast, &mut errors);

    for phase in &ast.phases {
        validate_phase(phase, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors { errors })
    }
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
    if let Some(on_error) = &config.on_error {
        validate_on_error(on_error, config.span.line, config.span.col, errors);
    }
}

fn validate_on_error(value: &str, line: usize, col: usize, errors: &mut Vec<DslError>) {
    if crate::compiler::parse_error_strategy(value).is_none() {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "invalid on_error value \"{value}\", expected \"continue\", \"abort\", or \"skip\""
            ),
        ));
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
            match run_if.trigger.as_str() {
                "modified" | "completed" => {}
                other => {
                    errors.push(DslError::validation(
                        phase.span.line,
                        phase.span.col,
                        format!(
                            "invalid run_if trigger \"{other}\", expected \"modified\" or \"completed\""
                        ),
                    ));
                }
            }
        }
    }
}

fn validate_cycle_detection(ast: &PolicyAst, errors: &mut Vec<DslError>) {
    // Build adjacency list
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for phase in &ast.phases {
        let deps: Vec<&str> = phase.depends_on.iter().map(|s| s.as_str()).collect();
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

fn validate_phase(phase: &PhaseNode, errors: &mut Vec<DslError>) {
    if let Some(on_error) = &phase.on_error {
        validate_on_error(on_error, phase.span.line, phase.span.col, errors);
    }

    // Track keep/remove conflicts (target, has_filter)
    let mut kept_targets: Vec<(&str, bool)> = Vec::new();
    let mut removed_targets: Vec<(&str, bool)> = Vec::new();

    for spanned_op in &phase.operations {
        validate_operation(
            &spanned_op.node,
            spanned_op.span.line,
            spanned_op.span.col,
            errors,
        );

        match &spanned_op.node {
            OperationNode::Keep { target, filter } => {
                kept_targets.push((target.as_str(), filter.is_some()));
            }
            OperationNode::Remove { target, filter } => {
                removed_targets.push((target.as_str(), filter.is_some()));
            }
            _ => {}
        }
    }

    // Detect set_tag / delete_tag conflicts
    let mut set_tag_keys: HashSet<&str> = HashSet::new();
    let mut delete_tag_keys: HashSet<&str> = HashSet::new();
    let mut has_clear_tags = false;
    let mut clear_tags_index: Option<usize> = None;

    for (i, spanned_op) in phase.operations.iter().enumerate() {
        match &spanned_op.node {
            OperationNode::ClearTags => {
                has_clear_tags = true;
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
    if has_clear_tags && !set_tag_keys.is_empty() {
        let clear_idx = clear_tags_index.unwrap();
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

    // Check for unfiltered keep+remove on the same broad target category
    for &(target, keep_filtered) in &kept_targets {
        let broad_category = broad_track_category(target);
        for &(removed, remove_filtered) in &removed_targets {
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

fn validate_operation(op: &OperationNode, line: usize, col: usize, errors: &mut Vec<DslError>) {
    match op {
        OperationNode::Container(name) => {
            if voom_domain::Container::from_extension(name) == voom_domain::Container::Other {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!("unknown container format \"{name}\""),
                ));
            }
        }
        OperationNode::Keep { target, filter } | OperationNode::Remove { target, filter } => {
            validate_track_target(target, line, col, errors);
            if let Some(f) = filter {
                validate_filter(f, line, col, errors);
            }
        }
        OperationNode::Transcode {
            target,
            codec,
            settings,
        } => {
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
            } else {
                reject_video_only_keys(settings, target, line, col, errors);
            }
        }
        OperationNode::Synthesize { settings, .. } => {
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
                        validate_filter(f, line, col, errors);
                    }
                    _ => {}
                }
            }
        }
        OperationNode::When(when) => {
            validate_when(when, line, col, errors);
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
                validate_when(&rule.when, line, col, errors);
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
        OperationNode::Actions { target, .. } => {
            validate_track_target(target, line, col, errors);
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

fn validate_field_path(path: &[String], line: usize, col: usize, errors: &mut Vec<DslError>) {
    if path.is_empty() {
        errors.push(DslError::validation(
            line,
            col,
            "empty field path in filter".to_string(),
        ));
        return;
    }
    if !KNOWN_FIELD_ROOTS.contains(&path[0].as_str()) {
        errors.push(DslError::validation(
            line,
            col,
            format!(
                "unknown field root \"{}\" in filter; \
                 expected one of: {}",
                path[0],
                KNOWN_FIELD_ROOTS.join(", ")
            ),
        ));
    }
}

fn validate_filter(filter: &FilterNode, line: usize, col: usize, errors: &mut Vec<DslError>) {
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
        FilterNode::LangField(_, path) => {
            validate_field_path(path, line, col, errors);
        }
        FilterNode::CodecIn(codecs_list) => {
            for codec in codecs_list {
                validate_codec(codec, line, col, errors);
            }
        }
        FilterNode::CodecCompare(_, codec) => {
            validate_codec(codec, line, col, errors);
        }
        FilterNode::CodecField(_, path) => {
            validate_field_path(path, line, col, errors);
        }
        FilterNode::And(items) | FilterNode::Or(items) => {
            for item in items {
                validate_filter(item, line, col, errors);
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
        FilterNode::Not(inner) => validate_filter(inner, line, col, errors),
        _ => {}
    }
}

fn validate_value(val: &Value, line: usize, col: usize, errors: &mut Vec<DslError>) {
    if let Value::Number(_, raw) = val {
        validate_number_suffix(raw, line, col, errors);
    }
}

fn validate_number_suffix(raw: &str, line: usize, col: usize, errors: &mut Vec<DslError>) {
    let suffix: String = raw
        .chars()
        .skip_while(|c| c.is_ascii_digit() || *c == '.')
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
    "channels",
    "hw",
    "hw_fallback",
    "max_resolution",
    "scale_algorithm",
    "hdr_mode",
    "tune",
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
        if !KNOWN_TRANSCODE_KEYS.contains(&key.as_str()) {
            let mut best: Option<(&str, usize)> = None;
            for &known in KNOWN_TRANSCODE_KEYS {
                let dist = edit_distance(key, known);
                if dist <= 3 && best.as_ref().map_or(true, |b| dist < b.1) {
                    best = Some((known, dist));
                }
            }
            if let Some((suggestion, _)) = best {
                errors.push(DslError::validation_with_suggestion(
                    line,
                    col,
                    format!(
                        "unknown transcode setting \"{key}\", \
                         expected one of: {}",
                        KNOWN_TRANSCODE_KEYS.join(", ")
                    ),
                    format!("did you mean \"{suggestion}\"?"),
                ));
            } else {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!(
                        "unknown transcode setting \"{key}\", \
                         expected one of: {}",
                        KNOWN_TRANSCODE_KEYS.join(", ")
                    ),
                ));
            }
        }
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

const VIDEO_ONLY_KEYS: &[&str] = &["hdr_mode", "tune", "scale_algorithm", "max_resolution"];

fn reject_video_only_keys(
    settings: &[(String, Value)],
    target: &str,
    line: usize,
    col: usize,
    errors: &mut Vec<DslError>,
) {
    for (key, _) in settings {
        if VIDEO_ONLY_KEYS.contains(&key.as_str()) {
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
    for (key, val) in settings {
        match key.as_str() {
            "hdr_mode" => {
                validate_ident_setting(val, "hdr_mode", VALID_HDR_MODES, line, col, errors);
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
            _ => {}
        }
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
        if !valid.contains(&name) {
            let valid_list = valid.join(", ");
            let mut best: Option<(&str, usize)> = None;
            for &v in valid {
                let dist = edit_distance(name, v);
                if dist <= 3 && best.as_ref().map_or(true, |b| dist < b.1) {
                    best = Some((v, dist));
                }
            }
            if let Some((suggestion, _)) = best {
                errors.push(DslError::validation_with_suggestion(
                    line,
                    col,
                    format!(
                        "unknown {key} value \"{name}\", \
                         expected one of: {valid_list}"
                    ),
                    format!("did you mean \"{suggestion}\"?"),
                ));
            } else {
                errors.push(DslError::validation(
                    line,
                    col,
                    format!(
                        "unknown {key} value \"{name}\", \
                         expected one of: {valid_list}"
                    ),
                ));
            }
        }
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
                    _ => "unknown",
                }
            ),
        ));
    }
}

fn validate_when(when: &WhenNode, line: usize, col: usize, errors: &mut Vec<DslError>) {
    validate_condition(&when.condition, line, col, errors);
}

fn validate_condition(cond: &ConditionNode, line: usize, col: usize, errors: &mut Vec<DslError>) {
    match cond {
        ConditionNode::Exists(query) | ConditionNode::Count(query, _, _) => {
            validate_track_target(&query.target, line, col, errors);
            if let Some(f) = &query.filter {
                validate_filter(f, line, col, errors);
            }
        }
        ConditionNode::And(items) | ConditionNode::Or(items) => {
            for item in items {
                validate_condition(item, line, col, errors);
            }
        }
        ConditionNode::Not(inner) => validate_condition(inner, line, col, errors),
        _ => {}
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
        let ast = parse_policy(input).unwrap();
        let err = validate(&ast).unwrap_err();
        assert!(format!("{}", err.errors[0]).contains("invalid on_error"));
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
        assert!(format!("{}", err.errors[0]).contains("unknown container format \"zzz\""));
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
}
