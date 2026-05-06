//! Pretty-printer for VOOM DSL ASTs.
//!
//! Converts a [`PolicyAst`] back into formatted source text.
//! Used for `voom policy fmt` and round-trip testing.

use std::fmt::Write;

use crate::ast::{
    ActionNode, CompareOp, ConditionNode, ConfigNode, FilterNode, OperationNode, PhaseNode,
    PolicyAst, RuleNode, SynthSetting, TrackQueryNode, Value, ValueOrField, WhenNode,
};

/// Format a [`PolicyAst`] into a pretty-printed source string.
///
/// # Examples
///
/// ```
/// use voom_dsl::{parse_policy, format_policy};
///
/// let ast = parse_policy(r#"policy "demo" {
///     phase init {
///         container mkv
///     }
/// }"#).unwrap();
///
/// let formatted = format_policy(&ast);
/// assert!(formatted.contains("policy \"demo\""));
/// assert!(formatted.contains("container mkv"));
/// ```
#[must_use]
pub fn format_policy(ast: &PolicyAst) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "policy \"{}\" {{", escape_string(&ast.name));

    if let Some(config) = &ast.config {
        format_config(config, &mut out, 1);
    }

    for (i, phase) in ast.phases.iter().enumerate() {
        if i > 0 || ast.config.is_some() {
            out.push('\n');
        }
        format_phase(phase, &mut out, 1);
    }

    out.push_str("}\n");
    out
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn format_config(config: &ConfigNode, out: &mut String, level: usize) {
    indent(out, level);
    out.push_str("config {\n");

    if !config.audio_languages.is_empty() {
        indent(out, level + 1);
        let _ = writeln!(
            out,
            "languages audio: [{}]",
            config.audio_languages.join(", ")
        );
    }
    if !config.subtitle_languages.is_empty() {
        indent(out, level + 1);
        let _ = writeln!(
            out,
            "languages subtitle: [{}]",
            config.subtitle_languages.join(", ")
        );
    }
    if !config.commentary_patterns.is_empty() {
        indent(out, level + 1);
        let patterns: Vec<String> = config
            .commentary_patterns
            .iter()
            .map(|p| format!("\"{}\"", escape_string(p)))
            .collect();
        let _ = writeln!(out, "commentary_patterns: [{}]", patterns.join(", "));
    }
    if let Some(on_error) = &config.on_error {
        indent(out, level + 1);
        let _ = writeln!(out, "on_error: {on_error}");
    }
    if let Some(val) = config.keep_backups {
        indent(out, level + 1);
        let _ = writeln!(out, "keep_backups: {val}");
    }

    indent(out, level);
    out.push_str("}\n");
}

fn format_phase(phase: &PhaseNode, out: &mut String, level: usize) {
    indent(out, level);
    let _ = writeln!(out, "phase {} {{", phase.name);

    if !phase.depends_on.is_empty() {
        indent(out, level + 1);
        let _ = writeln!(out, "depends_on: [{}]", phase.depends_on.join(", "));
    }

    if let Some(skip_when) = &phase.skip_when {
        indent(out, level + 1);
        out.push_str("skip when ");
        format_condition(skip_when, out);
        out.push('\n');
    }

    if let Some(run_if) = &phase.run_if {
        indent(out, level + 1);
        let _ = writeln!(out, "run_if {}.{}", run_if.phase, run_if.trigger);
    }

    if let Some(on_error) = &phase.on_error {
        indent(out, level + 1);
        let _ = writeln!(out, "on_error: {on_error}");
    }

    for spanned_op in &phase.operations {
        format_operation(&spanned_op.node, out, level + 1);
    }

    indent(out, level);
    out.push_str("}\n");
}

fn format_operation(op: &OperationNode, out: &mut String, level: usize) {
    match op {
        OperationNode::Container(name) => {
            indent(out, level);
            let _ = writeln!(out, "container {name}");
        }
        OperationNode::Keep { target, filter } => {
            indent(out, level);
            let _ = write!(out, "keep {target}");
            if let Some(f) = filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        OperationNode::Remove { target, filter } => {
            indent(out, level);
            let _ = write!(out, "remove {target}");
            if let Some(f) = filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        OperationNode::Order(items) => {
            indent(out, level);
            let _ = writeln!(out, "order tracks [{}]", items.join(", "));
        }
        OperationNode::Defaults(items) => {
            indent(out, level);
            out.push_str("defaults {\n");
            for (kind, value) in items {
                indent(out, level + 1);
                let _ = writeln!(out, "{kind}: {value}");
            }
            indent(out, level);
            out.push_str("}\n");
        }
        OperationNode::Actions { target, settings } => {
            indent(out, level);
            let _ = writeln!(out, "{target} actions {{");
            for (key, val) in settings {
                indent(out, level + 1);
                let _ = write!(out, "{key}: ");
                format_value(val, out);
                out.push('\n');
            }
            indent(out, level);
            out.push_str("}\n");
        }
        OperationNode::Transcode {
            target,
            codec,
            settings,
        } => format_transcode(target, codec, settings, out, level),
        OperationNode::Synthesize { name, settings } => {
            indent(out, level);
            let _ = writeln!(out, "synthesize \"{}\" {{", escape_string(name));
            for setting in settings {
                format_synth_setting(setting, out, level + 1);
            }
            indent(out, level);
            out.push_str("}\n");
        }
        OperationNode::ClearTags => {
            indent(out, level);
            out.push_str("clear_tags\n");
        }
        OperationNode::SetTag { tag, value } => {
            indent(out, level);
            let _ = write!(out, "set_tag \"{}\" ", escape_string(tag));
            format_value_or_field(value, out);
            out.push('\n');
        }
        OperationNode::DeleteTag(tag) => {
            indent(out, level);
            let _ = writeln!(out, "delete_tag \"{}\"", escape_string(tag));
        }
        OperationNode::When(when) => {
            format_when(when, out, level);
        }
        OperationNode::Rules { mode, rules } => format_rules(mode, rules, out, level),
    }
}

fn format_transcode(
    target: &str,
    codec: &str,
    settings: &[(String, Value)],
    out: &mut String,
    level: usize,
) {
    indent(out, level);
    if settings.is_empty() {
        let _ = writeln!(out, "transcode {target} to {codec}");
    } else {
        let _ = writeln!(out, "transcode {target} to {codec} {{");
        for (key, val) in settings {
            indent(out, level + 1);
            let _ = write!(out, "{key}: ");
            format_value(val, out);
            out.push('\n');
        }
        indent(out, level);
        out.push_str("}\n");
    }
}

fn format_rules(mode: &str, rules: &[RuleNode], out: &mut String, level: usize) {
    indent(out, level);
    let _ = writeln!(out, "rules {mode} {{");
    for rule in rules {
        indent(out, level + 1);
        let _ = writeln!(out, "rule \"{}\" {{", escape_string(&rule.name));
        format_when(&rule.when, out, level + 2);
        indent(out, level + 1);
        out.push_str("}\n");
    }
    indent(out, level);
    out.push_str("}\n");
}

fn format_synth_setting(setting: &SynthSetting, out: &mut String, level: usize) {
    indent(out, level);
    match setting {
        SynthSetting::Codec(c) => {
            let _ = writeln!(out, "codec: {c}");
        }
        SynthSetting::Channels(v) => {
            out.push_str("channels: ");
            format_value(v, out);
            out.push('\n');
        }
        SynthSetting::Source(f) => {
            out.push_str("source: prefer(");
            format_filter(f, out);
            out.push_str(")\n");
        }
        SynthSetting::Bitrate(b) => {
            let _ = writeln!(out, "bitrate: \"{}\"", escape_string(b));
        }
        SynthSetting::SkipIfExists(f) => {
            out.push_str("skip_if_exists { ");
            format_filter(f, out);
            out.push_str(" }\n");
        }
        SynthSetting::CreateIf(c) => {
            out.push_str("create_if ");
            format_condition(c, out);
            out.push('\n');
        }
        SynthSetting::Title(t) => {
            let _ = writeln!(out, "title: \"{}\"", escape_string(t));
        }
        SynthSetting::Language(l) => {
            let _ = writeln!(out, "language: {l}");
        }
        SynthSetting::Position(v) => {
            out.push_str("position: ");
            format_value(v, out);
            out.push('\n');
        }
    }
}

fn format_when(when: &WhenNode, out: &mut String, level: usize) {
    indent(out, level);
    out.push_str("when ");
    format_condition(&when.condition, out);
    out.push_str(" {\n");
    for action in &when.then_actions {
        format_action(action, out, level + 1);
    }
    if !when.else_actions.is_empty() {
        indent(out, level);
        out.push_str("} else {\n");
        for action in &when.else_actions {
            format_action(action, out, level + 1);
        }
    }
    indent(out, level);
    out.push_str("}\n");
}

fn format_condition(cond: &ConditionNode, out: &mut String) {
    match cond {
        ConditionNode::Exists(query) => {
            out.push_str("exists(");
            format_track_query(query, out);
            out.push(')');
        }
        ConditionNode::Count(query, op, value) => {
            out.push_str("count(");
            format_track_query(query, out);
            out.push_str(") ");
            format_compare_op(op, out);
            out.push(' ');
            format_number(*value, out);
        }
        ConditionNode::FieldCompare(path, op, value) => {
            out.push_str(&path.join("."));
            out.push(' ');
            format_compare_op(op, out);
            out.push(' ');
            format_value(value, out);
        }
        ConditionNode::FieldExists(path) => {
            out.push_str(&path.join("."));
            out.push_str(" exists");
        }
        ConditionNode::AudioIsMultiLanguage => out.push_str("audio_is_multi_language"),
        ConditionNode::IsDubbed => out.push_str("is_dubbed"),
        ConditionNode::IsOriginal => out.push_str("is_original"),
        ConditionNode::And(items) => format_and_condition(items, out),
        ConditionNode::Or(items) => format_or_condition(items, out),
        ConditionNode::Not(inner) => format_not_condition(inner, out),
    }
}

fn format_and_condition(items: &[ConditionNode], out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(" and ");
        }
        let needs_parens = matches!(item, ConditionNode::Or(_));
        if needs_parens {
            out.push('(');
        }
        format_condition(item, out);
        if needs_parens {
            out.push(')');
        }
    }
}

fn format_or_condition(items: &[ConditionNode], out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(" or ");
        }
        format_condition(item, out);
    }
}

fn format_not_condition(inner: &ConditionNode, out: &mut String) {
    out.push_str("not ");
    let needs_parens = matches!(inner, ConditionNode::And(_) | ConditionNode::Or(_));
    if needs_parens {
        out.push('(');
    }
    format_condition(inner, out);
    if needs_parens {
        out.push(')');
    }
}

fn format_track_query(query: &TrackQueryNode, out: &mut String) {
    out.push_str(&query.target);
    if let Some(f) = &query.filter {
        out.push_str(" where ");
        format_filter(f, out);
    }
}

fn format_filter(filter: &FilterNode, out: &mut String) {
    match filter {
        FilterNode::LangIn(langs) => {
            let _ = write!(out, "lang in [{}]", langs.join(", "));
        }
        FilterNode::LangCompare(op, lang) => {
            format_field_compare("lang", op, lang, out);
        }
        FilterNode::LangField(op, path) => {
            format_field_compare("lang", op, &path.join("."), out);
        }
        FilterNode::CodecIn(codecs) => {
            let _ = write!(out, "codec in [{}]", codecs.join(", "));
        }
        FilterNode::CodecCompare(op, codec) => {
            format_field_compare("codec", op, codec, out);
        }
        FilterNode::CodecField(op, path) => {
            format_field_compare("codec", op, &path.join("."), out);
        }
        FilterNode::Channels(op, val) => {
            out.push_str("channels ");
            format_compare_op(op, out);
            out.push(' ');
            format_number(*val, out);
        }
        FilterNode::Commentary => out.push_str("commentary"),
        FilterNode::Forced => out.push_str("forced"),
        FilterNode::Default => out.push_str("default"),
        FilterNode::Font => out.push_str("font"),
        FilterNode::TitleContains(s) => {
            let _ = write!(out, "title contains \"{}\"", escape_string(s));
        }
        FilterNode::TitleMatches(s) => {
            let _ = write!(out, "title matches \"{}\"", escape_string(s));
        }
        FilterNode::And(items) => format_filter_and(items, out),
        FilterNode::Or(items) => format_filter_or(items, out),
        FilterNode::Not(inner) => format_filter_not(inner, out),
    }
}

fn format_field_compare(field: &str, op: &CompareOp, value: &str, out: &mut String) {
    out.push_str(field);
    out.push(' ');
    format_compare_op(op, out);
    out.push(' ');
    out.push_str(value);
}

fn format_filter_and(items: &[FilterNode], out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(" and ");
        }
        let needs_parens = matches!(item, FilterNode::Or(_));
        if needs_parens {
            out.push('(');
        }
        format_filter(item, out);
        if needs_parens {
            out.push(')');
        }
    }
}

fn format_filter_or(items: &[FilterNode], out: &mut String) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(" or ");
        }
        format_filter(item, out);
    }
}

fn format_filter_not(inner: &FilterNode, out: &mut String) {
    out.push_str("not ");
    let needs_parens = matches!(inner, FilterNode::And(_) | FilterNode::Or(_));
    if needs_parens {
        out.push('(');
    }
    format_filter(inner, out);
    if needs_parens {
        out.push(')');
    }
}

fn format_action(action: &ActionNode, out: &mut String, level: usize) {
    indent(out, level);
    match action {
        ActionNode::Keep { target, filter } => {
            let _ = write!(out, "keep {target}");
            if let Some(f) = filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        ActionNode::Remove { target, filter } => {
            let _ = write!(out, "remove {target}");
            if let Some(f) = filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        ActionNode::Skip(phase) => {
            out.push_str("skip");
            if let Some(p) = phase {
                let _ = write!(out, " {p}");
            }
            out.push('\n');
        }
        ActionNode::Warn(msg) => {
            let _ = writeln!(out, "warn \"{}\"", escape_string(msg));
        }
        ActionNode::Fail(msg) => {
            let _ = writeln!(out, "fail \"{}\"", escape_string(msg));
        }
        ActionNode::SetDefault(track_ref) => {
            let _ = write!(out, "set_default {}", track_ref.target);
            if let Some(f) = &track_ref.filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        ActionNode::SetForced(track_ref) => {
            let _ = write!(out, "set_forced {}", track_ref.target);
            if let Some(f) = &track_ref.filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        ActionNode::SetLanguage(track_ref, val) => {
            let _ = write!(out, "set_language {}", track_ref.target);
            if let Some(f) = &track_ref.filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push(' ');
            format_value_or_field(val, out);
            out.push('\n');
        }
        ActionNode::SetTag(tag, val) => {
            let _ = write!(out, "set_tag \"{}\" ", escape_string(tag));
            format_value_or_field(val, out);
            out.push('\n');
        }
    }
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn format_value(val: &Value, out: &mut String) {
    match val {
        Value::String(s) => {
            let _ = write!(out, "\"{}\"", escape_string(s));
        }
        Value::Number(_, raw) => out.push_str(raw),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Ident(s) => out.push_str(s),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                format_value(item, out);
            }
            out.push(']');
        }
    }
}

fn format_value_or_field(val: &ValueOrField, out: &mut String) {
    match val {
        ValueOrField::Value(v) => format_value(v, out),
        ValueOrField::Field(path) => out.push_str(&path.join(".")),
    }
}

fn format_compare_op(op: &CompareOp, out: &mut String) {
    out.push_str(match op {
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Lt => "<",
        CompareOp::Le => "<=",
        CompareOp::Gt => ">",
        CompareOp::Ge => ">=",
        CompareOp::In => "in",
    });
}

fn format_number(n: f64, out: &mut String) {
    if (n - n.floor()).abs() < f64::EPSILON {
        // DSL numeric literals are small integers; truncation and sign loss are intentional.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let _ = write!(out, "{}", n as i64);
    } else {
        let _ = write!(out, "{n}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_policy;

    #[test]
    fn test_format_minimal() {
        let ast = parse_policy(
            r#"policy "test" {
            phase init {
                container mkv
            }
        }"#,
        )
        .unwrap();

        let formatted = format_policy(&ast);
        assert!(formatted.contains("policy \"test\""));
        assert!(formatted.contains("phase init"));
        assert!(formatted.contains("container mkv"));
    }

    #[test]
    fn test_roundtrip_minimal() {
        let input = r#"policy "test" {
            phase init {
                container mkv
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(ast1.name, ast2.name);
        assert_eq!(ast1.phases.len(), ast2.phases.len());
        assert_eq!(ast1.phases[0].name, ast2.phases[0].name);
    }

    #[test]
    fn test_roundtrip_with_config() {
        let input = r#"policy "test" {
            config {
                languages audio: [eng, und]
                languages subtitle: [eng]
                commentary_patterns: ["commentary", "director"]
                on_error: continue
            }
            phase init { container mkv }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();

        let c1 = ast1.config.unwrap();
        let c2 = ast2.config.unwrap();
        assert_eq!(c1.audio_languages, c2.audio_languages);
        assert_eq!(c1.subtitle_languages, c2.subtitle_languages);
        assert_eq!(c1.on_error, c2.on_error);
        assert_eq!(c1.commentary_patterns, c2.commentary_patterns);
    }

    #[test]
    fn test_roundtrip_keep_remove() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang in [eng, jpn]
                remove attachments where not font
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(
            ast1.phases[0].operations.len(),
            ast2.phases[0].operations.len()
        );
    }

    #[test]
    fn test_roundtrip_transcode() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crf: 20
                    preset: medium
                }
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();

        match (
            &ast1.phases[0].operations[0].node,
            &ast2.phases[0].operations[0].node,
        ) {
            (
                OperationNode::Transcode {
                    codec: c1,
                    settings: s1,
                    ..
                },
                OperationNode::Transcode {
                    codec: c2,
                    settings: s2,
                    ..
                },
            ) => {
                assert_eq!(c1, c2);
                assert_eq!(s1.len(), s2.len());
            }
            _ => panic!("expected Transcode"),
        }
    }

    #[test]
    fn test_roundtrip_when_block() {
        let input = r#"policy "test" {
            phase validate {
                when exists(audio where lang == jpn) {
                    warn "has japanese audio"
                }
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();

        match (
            &ast1.phases[0].operations[0].node,
            &ast2.phases[0].operations[0].node,
        ) {
            (OperationNode::When(w1), OperationNode::When(w2)) => {
                assert_eq!(w1.then_actions.len(), w2.then_actions.len());
            }
            _ => panic!("expected When"),
        }
    }

    #[test]
    fn test_roundtrip_production_policy() {
        let input = include_str!("../tests/fixtures/production-normalize.voom");
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();

        assert_eq!(ast1.name, ast2.name);
        assert_eq!(ast1.phases.len(), ast2.phases.len());
        for (p1, p2) in ast1.phases.iter().zip(ast2.phases.iter()) {
            assert_eq!(p1.name, p2.name);
            assert_eq!(p1.depends_on, p2.depends_on);
            assert_eq!(p1.operations.len(), p2.operations.len());
        }
    }

    #[test]
    fn test_roundtrip_set_language_no_filter() {
        // set_language without a filter should round-trip without "where"
        let input = r#"policy "test" {
            phase metadata {
                when is_dubbed {
                    set_language audio "eng"
                }
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(!formatted.contains("set_language audio where "));
        assert!(formatted.contains("set_language audio \"eng\""));
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(
            ast1.phases[0].operations.len(),
            ast2.phases[0].operations.len()
        );
    }

    #[test]
    fn test_roundtrip_string_escape() {
        let input = r#"policy "test" {
            phase validate {
                when is_dubbed {
                    warn "contains \"quoted\" text"
                }
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains(r#"\"quoted\""#));
        let ast2 = parse_policy(&formatted).unwrap();
        match (
            &ast1.phases[0].operations[0].node,
            &ast2.phases[0].operations[0].node,
        ) {
            (OperationNode::When(w1), OperationNode::When(w2)) => {
                match (&w1.then_actions[0], &w2.then_actions[0]) {
                    (ActionNode::Warn(m1), ActionNode::Warn(m2)) => assert_eq!(m1, m2),
                    _ => panic!("expected Warn"),
                }
            }
            _ => panic!("expected When"),
        }
    }

    #[test]
    fn test_format_lang_ne_filter() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang != jpn
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains("lang != jpn"));
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(
            ast1.phases[0].operations.len(),
            ast2.phases[0].operations.len()
        );
    }

    #[test]
    fn test_roundtrip_clear_tags() {
        let input = r#"policy "test" {
            phase clean {
                clear_tags
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains("clear_tags"));
        let ast2 = parse_policy(&formatted).unwrap();
        assert!(matches!(
            &ast2.phases[0].operations[0].node,
            OperationNode::ClearTags
        ));
    }

    #[test]
    fn test_roundtrip_set_tag() {
        let input = r#"policy "test" {
            phase clean {
                set_tag "title" "My Movie"
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains(r#"set_tag "title" "My Movie""#));
        let ast2 = parse_policy(&formatted).unwrap();
        match &ast2.phases[0].operations[0].node {
            OperationNode::SetTag { tag, .. } => assert_eq!(tag, "title"),
            other => panic!("expected SetTag, got {other:?}"),
        }
    }

    #[test]
    fn test_roundtrip_delete_tag() {
        let input = r#"policy "test" {
            phase clean {
                delete_tag "encoder"
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains(r#"delete_tag "encoder""#));
        let ast2 = parse_policy(&formatted).unwrap();
        match &ast2.phases[0].operations[0].node {
            OperationNode::DeleteTag(tag) => assert_eq!(tag, "encoder"),
            other => panic!("expected DeleteTag, got {other:?}"),
        }
    }

    #[test]
    fn test_roundtrip_keep_backups_false() {
        let input = r#"policy "test" {
            config {
                languages audio: [eng]
                keep_backups: false
            }
            phase init { container mkv }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(
            formatted.contains("keep_backups: false"),
            "formatted output should contain keep_backups: false, got: {formatted}"
        );
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(ast2.config.as_ref().unwrap().keep_backups, Some(false),);
    }

    #[test]
    fn test_roundtrip_container_metadata_combined() {
        let input = r#"policy "test" {
            phase clean {
                clear_tags
                container mkv
                set_tag "title" "My Movie"
                delete_tag "encoder"
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(
            ast1.phases[0].operations.len(),
            ast2.phases[0].operations.len()
        );
    }

    #[test]
    fn test_roundtrip_field_filter() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == plugin.radarr.original_language
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains("plugin.radarr.original_language"));
        let ast2 = parse_policy(&formatted).unwrap();
        assert_eq!(
            ast1.phases[0].operations.len(),
            ast2.phases[0].operations.len()
        );
    }

    #[test]
    fn test_roundtrip_keep_in_when() {
        let input = r#"policy "test" {
            phase validate {
                when exists(audio where lang == jpn) {
                    keep audio where lang in [eng, jpn]
                }
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains("keep audio where lang in [eng, jpn]"));
        let ast2 = parse_policy(&formatted).unwrap();
        match (
            &ast1.phases[0].operations[0].node,
            &ast2.phases[0].operations[0].node,
        ) {
            (OperationNode::When(w1), OperationNode::When(w2)) => {
                assert_eq!(w1.then_actions.len(), w2.then_actions.len());
                assert!(matches!(&w2.then_actions[0], ActionNode::Keep { .. }));
            }
            _ => panic!("expected When"),
        }
    }

    #[test]
    fn test_roundtrip_remove_in_when() {
        let input = r#"policy "test" {
            phase validate {
                when is_dubbed {
                    remove audio where commentary
                } else {
                    warn "not dubbed"
                }
            }
        }"#;
        let ast1 = parse_policy(input).unwrap();
        let formatted = format_policy(&ast1);
        assert!(formatted.contains("remove audio where commentary"));
        assert!(formatted.contains("else"));
        let ast2 = parse_policy(&formatted).unwrap();
        match (
            &ast1.phases[0].operations[0].node,
            &ast2.phases[0].operations[0].node,
        ) {
            (OperationNode::When(w1), OperationNode::When(w2)) => {
                assert_eq!(w1.then_actions.len(), w2.then_actions.len());
                assert_eq!(w1.else_actions.len(), w2.else_actions.len());
                assert!(matches!(&w2.then_actions[0], ActionNode::Remove { .. }));
            }
            _ => panic!("expected When"),
        }
    }

    // ---- format_phase indent-arithmetic tests (issue #236, phase 2) ----
    // Each test exercises one optional section of format_phase by calling it
    // at level=1 and asserting the inner line is indented by exactly 4
    // spaces (two indent steps × 2 spaces). Both `+ to -` (gives 0 spaces)
    // and `+ to *` (gives 2 spaces) mutants on each `level + 1` site fail
    // the substring assertion.

    use crate::ast::{RunIfNode, Span, SpannedOperation};

    fn empty_phase(name: &str) -> PhaseNode {
        PhaseNode {
            name: name.into(),
            skip_when: None,
            depends_on: vec![],
            run_if: None,
            on_error: None,
            operations: vec![],
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
            },
        }
    }

    #[test]
    fn format_phase_depends_on_indents_one_deeper() {
        let mut phase = empty_phase("build");
        phase.depends_on = vec!["init".into()];
        let mut out = String::new();
        format_phase(&phase, &mut out, 1);
        assert!(
            out.contains("\n    depends_on: [init]\n"),
            "depends_on line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_phase_skip_when_indents_one_deeper() {
        let mut phase = empty_phase("build");
        phase.skip_when = Some(ConditionNode::IsDubbed);
        let mut out = String::new();
        format_phase(&phase, &mut out, 1);
        assert!(
            out.contains("\n    skip when "),
            "skip when line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_phase_run_if_indents_one_deeper() {
        let mut phase = empty_phase("build");
        phase.run_if = Some(RunIfNode {
            phase: "init".into(),
            trigger: "modified".into(),
        });
        let mut out = String::new();
        format_phase(&phase, &mut out, 1);
        assert!(
            out.contains("\n    run_if init.modified\n"),
            "run_if line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_phase_on_error_indents_one_deeper() {
        let mut phase = empty_phase("build");
        phase.on_error = Some("skip".into());
        let mut out = String::new();
        format_phase(&phase, &mut out, 1);
        assert!(
            out.contains("\n    on_error: skip\n"),
            "on_error line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_phase_operation_indents_one_deeper() {
        let mut phase = empty_phase("build");
        phase.operations = vec![SpannedOperation {
            node: OperationNode::Container("mkv".into()),
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
            },
        }];
        let mut out = String::new();
        format_phase(&phase, &mut out, 1);
        assert!(
            out.contains("\n    container mkv\n"),
            "container operation should be indented by 4 spaces; got:\n{out}"
        );
    }

    // ---- format_config indent-arithmetic tests (issue #236, phase 2) ----
    // Mirrors the cluster 7 (format_phase) recipe: each test exercises one
    // optional config section by calling format_config at level=1 and
    // asserting the inner line is indented by exactly 4 spaces. Both `+ to
    // -` (gives 0 spaces) and `+ to *` (gives 2 spaces) mutants on each
    // `level + 1` site fail the substring check.

    fn empty_config() -> ConfigNode {
        ConfigNode {
            audio_languages: vec![],
            subtitle_languages: vec![],
            on_error: None,
            commentary_patterns: vec![],
            keep_backups: None,
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
            },
        }
    }

    #[test]
    fn format_config_audio_languages_indents_one_deeper() {
        let mut config = empty_config();
        config.audio_languages = vec!["eng".into()];
        let mut out = String::new();
        format_config(&config, &mut out, 1);
        assert!(
            out.contains("\n    languages audio: [eng]\n"),
            "audio languages line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_config_subtitle_languages_indents_one_deeper() {
        let mut config = empty_config();
        config.subtitle_languages = vec!["eng".into()];
        let mut out = String::new();
        format_config(&config, &mut out, 1);
        assert!(
            out.contains("\n    languages subtitle: [eng]\n"),
            "subtitle languages line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_config_commentary_patterns_indents_one_deeper() {
        let mut config = empty_config();
        config.commentary_patterns = vec!["commentary".into()];
        let mut out = String::new();
        format_config(&config, &mut out, 1);
        assert!(
            out.contains("\n    commentary_patterns: [\"commentary\"]\n"),
            "commentary_patterns line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_config_on_error_indents_one_deeper() {
        let mut config = empty_config();
        config.on_error = Some("skip".into());
        let mut out = String::new();
        format_config(&config, &mut out, 1);
        assert!(
            out.contains("\n    on_error: skip\n"),
            "on_error line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_config_keep_backups_indents_one_deeper() {
        let mut config = empty_config();
        config.keep_backups = Some(true);
        let mut out = String::new();
        format_config(&config, &mut out, 1);
        assert!(
            out.contains("\n    keep_backups: true\n"),
            "keep_backups line should be indented by 4 spaces; got:\n{out}"
        );
    }

    // ---- format_rules indent-arithmetic tests (issue #236, phase 2) ----
    // Calls format_rules at level=1 with a single rule and asserts each of
    // the three indented inner lines (rule-open `rule "r1" {` at 4 spaces,
    // the recursive `when ...` at 6 spaces, and the rule-close `}` at 4
    // spaces) appears at the correct depth. The `+ to -` mutants on each
    // site cause usize subtraction underflow at level=1, panicking the test
    // before the assertion runs — that still counts as caught. The `+ to *`
    // mutants produce wrong indentation that fails the substring check.

    fn simple_rule(name: &str) -> RuleNode {
        RuleNode {
            name: name.into(),
            when: WhenNode {
                condition: ConditionNode::IsDubbed,
                then_actions: vec![],
                else_actions: vec![],
                span: Span {
                    start: 0,
                    end: 0,
                    line: 1,
                    col: 1,
                },
            },
        }
    }

    #[test]
    fn format_rules_rule_open_indents_two_levels() {
        let rules = vec![simple_rule("r1")];
        let mut out = String::new();
        format_rules("first_match", &rules, &mut out, 1);
        assert!(
            out.contains("\n    rule \"r1\" {\n"),
            "rule-open line should be indented by 4 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_rules_when_indents_three_levels() {
        let rules = vec![simple_rule("r1")];
        let mut out = String::new();
        format_rules("first_match", &rules, &mut out, 1);
        assert!(
            out.contains("\n      when "),
            "when line should be indented by 6 spaces; got:\n{out}"
        );
    }

    #[test]
    fn format_rules_rule_close_indents_two_levels() {
        let rules = vec![simple_rule("r1")];
        let mut out = String::new();
        format_rules("first_match", &rules, &mut out, 1);
        assert!(
            out.contains("\n    }\n"),
            "rule-close brace should be indented by 4 spaces; got:\n{out}"
        );
    }
}
