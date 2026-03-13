//! Pretty-printer for VOOM DSL ASTs.
//!
//! Converts a [`PolicyAst`] back into formatted source text.
//! Used for `voom policy fmt` and round-trip testing.

use crate::ast::*;

/// Format a [`PolicyAst`] into a pretty-printed source string.
#[must_use]
pub fn format_policy(ast: &PolicyAst) -> String {
    let mut out = String::new();
    out.push_str(&format!("policy \"{}\" {{\n", escape_string(&ast.name)));

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
        out.push_str(&format!(
            "languages audio: [{}]\n",
            config.audio_languages.join(", ")
        ));
    }
    if !config.subtitle_languages.is_empty() {
        indent(out, level + 1);
        out.push_str(&format!(
            "languages subtitle: [{}]\n",
            config.subtitle_languages.join(", ")
        ));
    }
    if !config.commentary_patterns.is_empty() {
        indent(out, level + 1);
        let patterns: Vec<String> = config
            .commentary_patterns
            .iter()
            .map(|p| format!("\"{}\"", escape_string(p)))
            .collect();
        out.push_str(&format!("commentary_patterns: [{}]\n", patterns.join(", ")));
    }
    if let Some(on_error) = &config.on_error {
        indent(out, level + 1);
        out.push_str(&format!("on_error: {on_error}\n"));
    }

    indent(out, level);
    out.push_str("}\n");
}

fn format_phase(phase: &PhaseNode, out: &mut String, level: usize) {
    indent(out, level);
    out.push_str(&format!("phase {} {{\n", phase.name));

    if !phase.depends_on.is_empty() {
        indent(out, level + 1);
        out.push_str(&format!("depends_on: [{}]\n", phase.depends_on.join(", ")));
    }

    if let Some(skip_when) = &phase.skip_when {
        indent(out, level + 1);
        out.push_str("skip when ");
        format_condition(skip_when, out);
        out.push('\n');
    }

    if let Some(run_if) = &phase.run_if {
        indent(out, level + 1);
        out.push_str(&format!("run_if {}.{}\n", run_if.phase, run_if.trigger));
    }

    if let Some(on_error) = &phase.on_error {
        indent(out, level + 1);
        out.push_str(&format!("on_error: {on_error}\n"));
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
            out.push_str(&format!("container {name}\n"));
        }
        OperationNode::Keep { target, filter } => {
            indent(out, level);
            out.push_str(&format!("keep {target}"));
            if let Some(f) = filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        OperationNode::Remove { target, filter } => {
            indent(out, level);
            out.push_str(&format!("remove {target}"));
            if let Some(f) = filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        OperationNode::Order(items) => {
            indent(out, level);
            out.push_str(&format!("order tracks [{}]\n", items.join(", ")));
        }
        OperationNode::Defaults(items) => {
            indent(out, level);
            out.push_str("defaults {\n");
            for (kind, value) in items {
                indent(out, level + 1);
                out.push_str(&format!("{kind}: {value}\n"));
            }
            indent(out, level);
            out.push_str("}\n");
        }
        OperationNode::Actions { target, settings } => {
            indent(out, level);
            out.push_str(&format!("{target} actions {{\n"));
            for (key, val) in settings {
                indent(out, level + 1);
                out.push_str(&format!("{key}: "));
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
        } => {
            indent(out, level);
            if settings.is_empty() {
                out.push_str(&format!("transcode {target} to {codec}\n"));
            } else {
                out.push_str(&format!("transcode {target} to {codec} {{\n"));
                for (key, val) in settings {
                    indent(out, level + 1);
                    out.push_str(&format!("{key}: "));
                    format_value(val, out);
                    out.push('\n');
                }
                indent(out, level);
                out.push_str("}\n");
            }
        }
        OperationNode::Synthesize { name, settings } => {
            indent(out, level);
            out.push_str(&format!("synthesize \"{}\" {{\n", escape_string(name)));
            for setting in settings {
                format_synth_setting(setting, out, level + 1);
            }
            indent(out, level);
            out.push_str("}\n");
        }
        OperationNode::When(when) => {
            format_when(when, out, level);
        }
        OperationNode::Rules { mode, rules } => {
            indent(out, level);
            out.push_str(&format!("rules {mode} {{\n"));
            for rule in rules {
                indent(out, level + 1);
                out.push_str(&format!("rule \"{}\" {{\n", escape_string(&rule.name)));
                format_when(&rule.when, out, level + 2);
                indent(out, level + 1);
                out.push_str("}\n");
            }
            indent(out, level);
            out.push_str("}\n");
        }
    }
}

fn format_synth_setting(setting: &SynthSetting, out: &mut String, level: usize) {
    indent(out, level);
    match setting {
        SynthSetting::Codec(c) => out.push_str(&format!("codec: {c}\n")),
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
        SynthSetting::Bitrate(b) => out.push_str(&format!("bitrate: \"{}\"\n", escape_string(b))),
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
        SynthSetting::Title(t) => out.push_str(&format!("title: \"{}\"\n", escape_string(t))),
        SynthSetting::Language(l) => out.push_str(&format!("language: {l}\n")),
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
        ConditionNode::And(items) => {
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
        ConditionNode::Or(items) => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(" or ");
                }
                format_condition(item, out);
            }
        }
        ConditionNode::Not(inner) => {
            out.push_str("not ");
            let needs_parens =
                matches!(inner.as_ref(), ConditionNode::And(_) | ConditionNode::Or(_));
            if needs_parens {
                out.push('(');
            }
            format_condition(inner, out);
            if needs_parens {
                out.push(')');
            }
        }
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
            if langs.len() == 1 {
                out.push_str(&format!("lang == {}", langs[0]));
            } else {
                out.push_str(&format!("lang in [{}]", langs.join(", ")));
            }
        }
        FilterNode::LangCompare(op, lang) => {
            out.push_str("lang ");
            format_compare_op(op, out);
            out.push(' ');
            out.push_str(lang);
        }
        FilterNode::CodecIn(codecs) => {
            if codecs.len() == 1 {
                out.push_str(&format!("codec == {}", codecs[0]));
            } else {
                out.push_str(&format!("codec in [{}]", codecs.join(", ")));
            }
        }
        FilterNode::CodecCompare(op, codec) => {
            out.push_str("codec ");
            format_compare_op(op, out);
            out.push(' ');
            out.push_str(codec);
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
            out.push_str(&format!("title contains \"{}\"", escape_string(s)))
        }
        FilterNode::TitleMatches(s) => {
            out.push_str(&format!("title matches \"{}\"", escape_string(s)))
        }
        FilterNode::And(items) => {
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
        FilterNode::Or(items) => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(" or ");
                }
                format_filter(item, out);
            }
        }
        FilterNode::Not(inner) => {
            out.push_str("not ");
            let needs_parens = matches!(inner.as_ref(), FilterNode::And(_) | FilterNode::Or(_));
            if needs_parens {
                out.push('(');
            }
            format_filter(inner, out);
            if needs_parens {
                out.push(')');
            }
        }
    }
}

fn format_action(action: &ActionNode, out: &mut String, level: usize) {
    indent(out, level);
    match action {
        ActionNode::Skip(phase) => {
            out.push_str("skip");
            if let Some(p) = phase {
                out.push_str(&format!(" {p}"));
            }
            out.push('\n');
        }
        ActionNode::Warn(msg) => out.push_str(&format!("warn \"{}\"\n", escape_string(msg))),
        ActionNode::Fail(msg) => out.push_str(&format!("fail \"{}\"\n", escape_string(msg))),
        ActionNode::SetDefault(track_ref) => {
            out.push_str(&format!("set_default {}", track_ref.target));
            if let Some(f) = &track_ref.filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        ActionNode::SetForced(track_ref) => {
            out.push_str(&format!("set_forced {}", track_ref.target));
            if let Some(f) = &track_ref.filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push('\n');
        }
        ActionNode::SetLanguage(track_ref, val) => {
            out.push_str(&format!("set_language {}", track_ref.target));
            if let Some(f) = &track_ref.filter {
                out.push_str(" where ");
                format_filter(f, out);
            }
            out.push(' ');
            format_value_or_field(val, out);
            out.push('\n');
        }
        ActionNode::SetTag(tag, val) => {
            out.push_str(&format!("set_tag \"{}\" ", escape_string(tag)));
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
        Value::String(s) => out.push_str(&format!("\"{}\"", escape_string(s))),
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
        out.push_str(&format!("{}", n as i64));
    } else {
        out.push_str(&format!("{n}"));
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
}
