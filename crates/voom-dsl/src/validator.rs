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

use voom_domain::utils::{codecs, language};

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
                ast.span.line,
                ast.span.col,
                format!("unknown audio language code \"{lang}\""),
            ));
        }
    }
    for lang in &config.subtitle_languages {
        if !language::is_valid_language(lang) {
            errors.push(DslError::validation(
                ast.span.line,
                ast.span.col,
                format!("unknown subtitle language code \"{lang}\""),
            ));
        }
    }
    if let Some(on_error) = &config.on_error {
        validate_on_error(on_error, ast.span.line, ast.span.col, errors);
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

    // Track keep/remove conflicts
    let mut kept_targets: HashSet<&str> = HashSet::new();
    let mut removed_targets: HashSet<&str> = HashSet::new();

    for spanned_op in &phase.operations {
        validate_operation(
            &spanned_op.node,
            spanned_op.span.line,
            spanned_op.span.col,
            errors,
        );

        match &spanned_op.node {
            OperationNode::Keep { target, .. } => {
                kept_targets.insert(target.as_str());
            }
            OperationNode::Remove { target, .. } => {
                removed_targets.insert(target.as_str());
            }
            _ => {}
        }
    }

    // Check for unfiltered keep+remove on the same broad target category
    for target in &kept_targets {
        let broad_category = broad_track_category(target);
        for removed in &removed_targets {
            if broad_track_category(removed) == broad_category {
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
            let valid_containers = ["mkv", "mp4", "avi", "webm", "mov", "ts"];
            if !valid_containers.contains(&name.as_str()) {
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
            for (_, val) in settings {
                validate_value(val, line, col, errors);
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
                validate_filter(item, line, col, errors);
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
    fn test_conflicting_keep_remove() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang in [eng]
                remove audio where lang in [jpn]
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
    fn test_conflicting_keep_remove_synonyms() {
        // "keep subtitles" + "remove subtitle" should conflict
        let input = r#"policy "test" {
            phase norm {
                keep subtitles where lang in [eng]
                remove subtitle where commentary
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
}
