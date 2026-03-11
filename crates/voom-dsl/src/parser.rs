//! Parser for the VOOM DSL.
//!
//! Uses pest to parse `.voom` source text into a CST, then converts
//! the CST into typed AST nodes defined in [`crate::ast`].

use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

use crate::ast::*;
use crate::errors::{DslError, Result};

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct VoomParser;

/// Parse a `.voom` source string into a [`PolicyAst`].
pub fn parse_policy(input: &str) -> Result<PolicyAst> {
    let pairs = VoomParser::parse(Rule::policy, input).map_err(|e| {
        let (line, col) = match e.line_col {
            pest::error::LineColLocation::Pos((l, c)) => (l, c),
            pest::error::LineColLocation::Span((l, c), _) => (l, c),
        };
        DslError::parse(line, col, format!("{e}"))
    })?;

    let pair = pairs.into_iter().next().unwrap();
    build_policy(pair)
}

fn span_from_pair(pair: &Pair<'_, Rule>) -> Span {
    let pest_span = pair.as_span();
    let (line, col) = pest_span.start_pos().line_col();
    Span::new(pest_span.start(), pest_span.end(), line, col)
}

fn build_policy(pair: Pair<'_, Rule>) -> Result<PolicyAst> {
    let span = span_from_pair(&pair);
    let mut inner = pair.into_inner();

    // Skip SOI
    // First meaningful token is the policy name string
    let name_pair = inner.next().unwrap();
    let name = parse_string_value(&name_pair);

    let mut config = None;
    let mut phases = Vec::new();

    for item in inner {
        match item.as_rule() {
            Rule::config => config = Some(build_config(item)?),
            Rule::phase => phases.push(build_phase(item)?),
            Rule::EOI => {}
            _ => {}
        }
    }

    Ok(PolicyAst {
        name,
        config,
        phases,
        span,
    })
}

fn build_config(pair: Pair<'_, Rule>) -> Result<ConfigNode> {
    let mut audio_languages = Vec::new();
    let mut subtitle_languages = Vec::new();
    let mut on_error = None;
    let mut commentary_patterns = Vec::new();

    for item in pair.into_inner() {
        if item.as_rule() != Rule::config_item {
            continue;
        }
        let text = item.as_str().trim();

        if text.starts_with("languages") {
            // pest silently consumes the "audio"|"subtitle" keyword as part of the
            // alternation but doesn't produce a named child for it. We detect it from text.
            let is_audio = text.contains("audio");
            // Find the list child
            let list_pair = item
                .into_inner()
                .find(|p| p.as_rule() == Rule::list)
                .unwrap();
            let values = build_list(&list_pair);
            if is_audio {
                audio_languages = values;
            } else {
                subtitle_languages = values;
            }
        } else if text.starts_with("on_error") {
            let ident_pair = item
                .into_inner()
                .find(|p| p.as_rule() == Rule::ident)
                .unwrap();
            on_error = Some(ident_pair.as_str().to_string());
        } else if text.starts_with("commentary_patterns") {
            let list_pair = item
                .into_inner()
                .find(|p| p.as_rule() == Rule::list)
                .unwrap();
            commentary_patterns = build_list(&list_pair);
        }
    }

    Ok(ConfigNode {
        audio_languages,
        subtitle_languages,
        on_error,
        commentary_patterns,
    })
}

fn build_phase(pair: Pair<'_, Rule>) -> Result<PhaseNode> {
    let span = span_from_pair(&pair);
    let mut inner = pair.into_inner();

    let name = inner.next().unwrap().as_str().to_string();

    let mut skip_when = None;
    let mut depends_on = Vec::new();
    let mut run_if = None;
    let mut on_error = None;
    let mut operations = Vec::new();

    for item in inner {
        if item.as_rule() != Rule::phase_item {
            continue;
        }
        let child = item.into_inner().next().unwrap();
        match child.as_rule() {
            Rule::skip_when => {
                let cond = child.into_inner().next().unwrap();
                skip_when = Some(build_condition(cond)?);
            }
            Rule::depends_on => {
                let list = child.into_inner().next().unwrap();
                depends_on = build_list(&list);
            }
            Rule::run_if => {
                // Grammar: "run_if" ~ ident ~ "." ~ ("modified" | "completed")
                // Only the ident is a named rule child; extract trigger from text.
                let text = child.as_str();
                let phase_name = child.into_inner().next().unwrap().as_str().to_string();
                let trigger = if text.contains("modified") {
                    "modified".to_string()
                } else {
                    "completed".to_string()
                };
                run_if = Some(RunIfNode {
                    phase: phase_name,
                    trigger,
                });
            }
            Rule::on_error => {
                let ident = child.into_inner().next().unwrap();
                on_error = Some(ident.as_str().to_string());
            }
            Rule::container_op => {
                let ident = child.into_inner().next().unwrap();
                operations.push(OperationNode::Container(ident.as_str().to_string()));
            }
            Rule::keep_op => operations.push(build_keep_remove(child, true)?),
            Rule::remove_op => operations.push(build_keep_remove(child, false)?),
            Rule::order_op => {
                let list = child.into_inner().next().unwrap();
                operations.push(OperationNode::Order(build_list(&list)));
            }
            Rule::defaults_op => operations.push(build_defaults(child)?),
            Rule::actions_op => operations.push(build_actions(child)?),
            Rule::transcode_op => operations.push(build_transcode(child)?),
            Rule::synthesize_op => operations.push(build_synthesize(child)?),
            Rule::when_block => operations.push(OperationNode::When(build_when(child)?)),
            Rule::rules_block => operations.push(build_rules(child)?),
            other => {
                let (line, col) = child.as_span().start_pos().line_col();
                return Err(DslError::unexpected_rule(format!("{other:?}"), line, col));
            }
        }
    }

    Ok(PhaseNode {
        name,
        skip_when,
        depends_on,
        run_if,
        on_error,
        operations,
        span,
    })
}

fn build_keep_remove(pair: Pair<'_, Rule>, is_keep: bool) -> Result<OperationNode> {
    let mut inner = pair.into_inner();
    let target = inner.next().unwrap().as_str().to_string();
    let filter = if let Some(where_pair) = inner.next() {
        let filter_pair = where_pair.into_inner().next().unwrap();
        Some(build_filter(filter_pair)?)
    } else {
        None
    };

    if is_keep {
        Ok(OperationNode::Keep { target, filter })
    } else {
        Ok(OperationNode::Remove { target, filter })
    }
}

fn build_defaults(pair: Pair<'_, Rule>) -> Result<OperationNode> {
    let mut items = Vec::new();
    for child in pair.into_inner() {
        if child.as_rule() == Rule::default_item {
            let text = child.as_str().trim();
            let kind = if text.starts_with("audio") {
                "audio".to_string()
            } else {
                "subtitle".to_string()
            };
            // Only the trailing ident is a named child
            let value = child.into_inner().next().unwrap().as_str().to_string();
            items.push((kind, value));
        }
    }
    Ok(OperationNode::Defaults(items))
}

fn build_actions(pair: Pair<'_, Rule>) -> Result<OperationNode> {
    let text = pair.as_str();
    let target = text.split_whitespace().next().unwrap().to_string();
    let mut settings = Vec::new();
    for child in pair.into_inner() {
        if child.as_rule() == Rule::action_setting {
            let mut parts = child.into_inner();
            let key = parts.next().unwrap().as_str().to_string();
            let val = build_value(parts.next().unwrap());
            settings.push((key, val));
        }
    }
    Ok(OperationNode::Actions { target, settings })
}

fn build_transcode(pair: Pair<'_, Rule>) -> Result<OperationNode> {
    let text = pair.as_str();
    // "transcode video to hevc { ... }" or "transcode audio to aac { ... }"
    let target = if text.contains("video") {
        "video"
    } else {
        "audio"
    }
    .to_string();

    let mut inner = pair.into_inner();
    let codec = inner.next().unwrap().as_str().to_string();
    let mut settings = Vec::new();

    if let Some(block_pair) = inner.next() {
        for child in block_pair.into_inner() {
            if child.as_rule() == Rule::kv_pair {
                let mut parts = child.into_inner();
                let key = parts.next().unwrap().as_str().to_string();
                let val = build_value(parts.next().unwrap());
                settings.push((key, val));
            }
        }
    }

    Ok(OperationNode::Transcode {
        target,
        codec,
        settings,
    })
}

fn build_synthesize(pair: Pair<'_, Rule>) -> Result<OperationNode> {
    let mut inner = pair.into_inner();
    let name = parse_string_value(&inner.next().unwrap());
    let mut settings = Vec::new();

    for child in inner {
        if child.as_rule() != Rule::synth_item {
            continue;
        }
        let text = child.as_str().trim();
        let mut parts = child.into_inner();

        if text.starts_with("codec") {
            let val = parts.next().unwrap().as_str().to_string();
            settings.push(SynthSetting::Codec(val));
        } else if text.starts_with("channels") {
            let val = build_value(parts.next().unwrap());
            settings.push(SynthSetting::Channels(val));
        } else if text.starts_with("source") {
            let source_pair = parts.next().unwrap();
            let filter = source_pair.into_inner().next().unwrap();
            settings.push(SynthSetting::Source(build_filter(filter)?));
        } else if text.starts_with("bitrate") {
            let val = parse_string_value(&parts.next().unwrap());
            settings.push(SynthSetting::Bitrate(val));
        } else if text.starts_with("skip_if_exists") {
            let filter = parts.next().unwrap();
            settings.push(SynthSetting::SkipIfExists(build_filter(filter)?));
        } else if text.starts_with("create_if") {
            let cond = parts.next().unwrap();
            settings.push(SynthSetting::CreateIf(build_condition(cond)?));
        } else if text.starts_with("title") {
            let val = parse_string_value(&parts.next().unwrap());
            settings.push(SynthSetting::Title(val));
        } else if text.starts_with("language") {
            let val = parts.next().unwrap().as_str().to_string();
            settings.push(SynthSetting::Language(val));
        } else if text.starts_with("position") {
            let val = build_value(parts.next().unwrap());
            settings.push(SynthSetting::Position(val));
        }
    }

    Ok(OperationNode::Synthesize { name, settings })
}

fn build_when(pair: Pair<'_, Rule>) -> Result<WhenNode> {
    let mut inner = pair.into_inner();
    let cond_pair = inner.next().unwrap();
    let condition = build_condition(cond_pair)?;

    let mut then_actions = Vec::new();
    let mut else_actions = Vec::new();

    for child in inner {
        match child.as_rule() {
            Rule::action => then_actions.push(build_action(child)?),
            Rule::else_block => {
                for action_pair in child.into_inner() {
                    if action_pair.as_rule() == Rule::action {
                        else_actions.push(build_action(action_pair)?);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(WhenNode {
        condition,
        then_actions,
        else_actions,
    })
}

fn build_rules(pair: Pair<'_, Rule>) -> Result<OperationNode> {
    let mut inner = pair.into_inner();
    let mode = inner.next().unwrap().as_str().to_string();
    let mut rules = Vec::new();

    for child in inner {
        if child.as_rule() == Rule::rule_item {
            let mut parts = child.into_inner();
            let name = parse_string_value(&parts.next().unwrap());
            let when_pair = parts.next().unwrap();
            rules.push(RuleNode {
                name,
                when: build_when(when_pair)?,
            });
        }
    }

    Ok(OperationNode::Rules { mode, rules })
}

fn build_condition(pair: Pair<'_, Rule>) -> Result<ConditionNode> {
    let inner = pair.into_inner().next().unwrap();
    build_condition_or(inner)
}

fn build_condition_or(pair: Pair<'_, Rule>) -> Result<ConditionNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_condition_and(parts.into_iter().next().unwrap());
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_condition_and(p)?);
    }
    Ok(ConditionNode::Or(nodes))
}

fn build_condition_and(pair: Pair<'_, Rule>) -> Result<ConditionNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_condition_not(parts.into_iter().next().unwrap());
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_condition_not(p)?);
    }
    Ok(ConditionNode::And(nodes))
}

fn build_condition_not(pair: Pair<'_, Rule>) -> Result<ConditionNode> {
    let text = pair.as_str().trim();
    let inner = pair.into_inner().next().unwrap();
    if text.starts_with("not") {
        Ok(ConditionNode::Not(Box::new(build_condition_atom(inner)?)))
    } else {
        build_condition_atom(inner)
    }
}

fn build_condition_atom(pair: Pair<'_, Rule>) -> Result<ConditionNode> {
    let text = pair.as_str().trim();
    let mut inner = pair.into_inner();

    if text.starts_with("audio_is_multi_language") {
        return Ok(ConditionNode::AudioIsMultiLanguage);
    }
    if text.starts_with("is_dubbed") {
        return Ok(ConditionNode::IsDubbed);
    }
    if text.starts_with("is_original") {
        return Ok(ConditionNode::IsOriginal);
    }
    if text.starts_with("exists") {
        let query = inner.next().unwrap();
        return Ok(ConditionNode::Exists(build_track_query(query)?));
    }
    if text.starts_with("count") {
        let query = inner.next().unwrap();
        let op = build_compare_op(inner.next().unwrap());
        let num = parse_number_f64(inner.next().unwrap());
        return Ok(ConditionNode::Count(build_track_query(query)?, op, num));
    }
    if text.starts_with('(') {
        let cond = inner.next().unwrap();
        return build_condition(cond);
    }

    // field_access compare_op value  OR  field_access exists
    let first = inner.next().unwrap();
    if first.as_rule() == Rule::field_access {
        let fields = build_field_access(&first);
        if let Some(second) = inner.next() {
            if second.as_rule() == Rule::compare_op {
                let op = build_compare_op(second);
                let val = build_value(inner.next().unwrap());
                return Ok(ConditionNode::FieldCompare(fields, op, val));
            }
        }
        // "field_access exists" — the "exists" keyword was consumed by pest
        return Ok(ConditionNode::FieldExists(fields));
    }

    let (line, col) = first.as_span().start_pos().line_col();
    Err(DslError::build(
        line,
        col,
        format!("unexpected condition: {text}"),
    ))
}

fn build_track_query(pair: Pair<'_, Rule>) -> Result<TrackQueryNode> {
    let mut inner = pair.into_inner();
    let target_pair = inner.next().unwrap();
    let target = target_pair.as_str().to_string();

    let filter = if let Some(filter_pair) = inner.next() {
        Some(build_filter(filter_pair)?)
    } else {
        None
    };

    Ok(TrackQueryNode { target, filter })
}

fn build_filter(pair: Pair<'_, Rule>) -> Result<FilterNode> {
    let inner = pair.into_inner().next().unwrap();
    build_filter_or(inner)
}

fn build_filter_or(pair: Pair<'_, Rule>) -> Result<FilterNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_filter_and(parts.into_iter().next().unwrap());
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_filter_and(p)?);
    }
    Ok(FilterNode::Or(nodes))
}

fn build_filter_and(pair: Pair<'_, Rule>) -> Result<FilterNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_filter_not(parts.into_iter().next().unwrap());
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_filter_not(p)?);
    }
    Ok(FilterNode::And(nodes))
}

fn build_filter_not(pair: Pair<'_, Rule>) -> Result<FilterNode> {
    let text = pair.as_str().trim();
    let inner = pair.into_inner().next().unwrap();
    if text.starts_with("not") {
        Ok(FilterNode::Not(Box::new(build_filter_atom(inner)?)))
    } else {
        build_filter_atom(inner)
    }
}

fn build_filter_atom(pair: Pair<'_, Rule>) -> Result<FilterNode> {
    let text = pair.as_str().trim();
    let span = pair.as_span();
    let mut inner = pair.into_inner();

    if text.starts_with("lang") {
        let next = inner.next().unwrap();
        if next.as_rule() == Rule::list {
            return Ok(FilterNode::LangIn(build_list(&next)));
        }
        // lang compare_op value — treat as lang == X  →  LangIn([X])
        let op = build_compare_op(next);
        let val = inner.next().unwrap();
        let val_str = match val.as_rule() {
            Rule::value => {
                let v = val.into_inner().next().unwrap();
                v.as_str().to_string()
            }
            _ => val.as_str().to_string(),
        };
        return match op {
            CompareOp::Eq => Ok(FilterNode::LangIn(vec![val_str])),
            CompareOp::In => Ok(FilterNode::LangIn(vec![val_str])),
            _ => Ok(FilterNode::LangIn(vec![val_str])),
        };
    }
    if text.starts_with("codec") {
        let next = inner.next().unwrap();
        if next.as_rule() == Rule::list {
            return Ok(FilterNode::CodecIn(build_list(&next)));
        }
        let op = build_compare_op(next);
        let val = inner.next().unwrap();
        let val_str = match val.as_rule() {
            Rule::value => {
                let v = val.into_inner().next().unwrap();
                v.as_str().to_string()
            }
            _ => val.as_str().to_string(),
        };
        return match op {
            CompareOp::Eq => Ok(FilterNode::CodecIn(vec![val_str])),
            CompareOp::In => Ok(FilterNode::CodecIn(vec![val_str])),
            _ => Ok(FilterNode::CodecIn(vec![val_str])),
        };
    }
    if text.starts_with("channels") {
        let op = build_compare_op(inner.next().unwrap());
        let num = parse_number_f64(inner.next().unwrap());
        return Ok(FilterNode::Channels(op, num));
    }
    if text == "commentary" {
        return Ok(FilterNode::Commentary);
    }
    if text == "forced" {
        return Ok(FilterNode::Forced);
    }
    if text == "default" {
        return Ok(FilterNode::Default);
    }
    if text == "font" {
        return Ok(FilterNode::Font);
    }
    if text.starts_with("title") {
        let has_contains = text.contains("contains");
        let str_pair = inner.next().unwrap();
        let s = parse_string_value(&str_pair);
        return if has_contains {
            Ok(FilterNode::TitleContains(s))
        } else {
            Ok(FilterNode::TitleMatches(s))
        };
    }
    if text.starts_with('(') {
        let filter = inner.next().unwrap();
        return build_filter(filter);
    }

    let (line, col) = span.start_pos().line_col();
    Err(DslError::build(
        line,
        col,
        format!("unexpected filter: {text}"),
    ))
}

fn build_action(pair: Pair<'_, Rule>) -> Result<ActionNode> {
    let text = pair.as_str().trim();
    let span = pair.as_span();
    let mut inner = pair.into_inner();

    if text.starts_with("skip") {
        let phase = inner.next().map(|p| p.as_str().to_string());
        return Ok(ActionNode::Skip(phase));
    }
    if text.starts_with("warn") {
        let s = parse_string_value(&inner.next().unwrap());
        return Ok(ActionNode::Warn(s));
    }
    if text.starts_with("fail") {
        let s = parse_string_value(&inner.next().unwrap());
        return Ok(ActionNode::Fail(s));
    }
    if text.starts_with("set_default") {
        let track = build_track_ref(inner.next().unwrap())?;
        return Ok(ActionNode::SetDefault(track));
    }
    if text.starts_with("set_forced") {
        let track = build_track_ref(inner.next().unwrap())?;
        return Ok(ActionNode::SetForced(track));
    }
    if text.starts_with("set_language") {
        let track = build_track_ref(inner.next().unwrap())?;
        let val_pair = inner.next().unwrap();
        let val = if val_pair.as_rule() == Rule::field_access {
            ValueOrField::Field(build_field_access(&val_pair))
        } else {
            ValueOrField::Value(Value::String(parse_string_value(&val_pair)))
        };
        return Ok(ActionNode::SetLanguage(track, val));
    }
    if text.starts_with("set_tag") {
        let tag = parse_string_value(&inner.next().unwrap());
        let val_pair = inner.next().unwrap();
        let val = if val_pair.as_rule() == Rule::field_access {
            ValueOrField::Field(build_field_access(&val_pair))
        } else {
            ValueOrField::Value(build_value(val_pair))
        };
        return Ok(ActionNode::SetTag(tag, val));
    }

    let (line, col) = span.start_pos().line_col();
    Err(DslError::build(
        line,
        col,
        format!("unexpected action: {text}"),
    ))
}

fn build_track_ref(pair: Pair<'_, Rule>) -> Result<TrackRefNode> {
    let mut inner = pair.into_inner();
    let target = inner.next().unwrap().as_str().to_string();
    let filter = if let Some(where_pair) = inner.next() {
        Some(build_filter(where_pair)?)
    } else {
        None
    };
    Ok(TrackRefNode { target, filter })
}

fn build_field_access(pair: &Pair<'_, Rule>) -> Vec<String> {
    pair.as_str()
        .split('.')
        .map(|s| s.trim().to_string())
        .collect()
}

fn build_compare_op(pair: Pair<'_, Rule>) -> CompareOp {
    match pair.as_str() {
        "==" => CompareOp::Eq,
        "!=" => CompareOp::Ne,
        "<" => CompareOp::Lt,
        "<=" => CompareOp::Le,
        ">" => CompareOp::Gt,
        ">=" => CompareOp::Ge,
        "in" => CompareOp::In,
        _ => CompareOp::Eq,
    }
}

fn build_value(pair: Pair<'_, Rule>) -> Value {
    match pair.as_rule() {
        Rule::value => {
            let inner = pair.into_inner().next().unwrap();
            build_value(inner)
        }
        Rule::string => Value::String(parse_string_value(&pair)),
        Rule::number => {
            let raw = pair.as_str().to_string();
            let num = parse_number_f64(pair);
            Value::Number(num, raw)
        }
        Rule::boolean => Value::Bool(pair.as_str() == "true"),
        Rule::ident => Value::Ident(pair.as_str().to_string()),
        Rule::list => Value::List(build_list_values(&pair)),
        _ => Value::Ident(pair.as_str().to_string()),
    }
}

fn build_list(pair: &Pair<'_, Rule>) -> Vec<String> {
    pair.clone()
        .into_inner()
        .map(|v| {
            let inner = v.into_inner().next().unwrap();
            match inner.as_rule() {
                Rule::string => parse_string_value(&inner),
                _ => inner.as_str().to_string(),
            }
        })
        .collect()
}

fn build_list_values(pair: &Pair<'_, Rule>) -> Vec<Value> {
    pair.clone().into_inner().map(build_value).collect()
}

/// Strip surrounding quotes from a string literal.
fn parse_string_value(pair: &Pair<'_, Rule>) -> String {
    let s = pair.as_str();
    if s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parse a number token, stripping any trailing unit suffix (e.g., "192k" → 192.0).
fn parse_number_f64(pair: Pair<'_, Rule>) -> f64 {
    let s = pair.as_str();
    let numeric: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    numeric.parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_policy() {
        let input = r#"policy "test" {
            phase init {
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert_eq!(ast.name, "test");
        assert_eq!(ast.phases.len(), 1);
        assert_eq!(ast.phases[0].name, "init");
    }

    #[test]
    fn test_parse_error_reports_location() {
        let input = r#"policy "test" {
            phase {
            }
        }"#;
        let err = parse_policy(input).unwrap_err();
        match err {
            DslError::Parse { line, .. } => assert!(line > 0),
            _ => panic!("expected parse error"),
        }
    }

    #[test]
    fn test_parse_config_block() {
        let input = r#"policy "test" {
            config {
                languages audio: [eng, und]
                languages subtitle: [eng]
                on_error: continue
                commentary_patterns: ["commentary", "director"]
            }
            phase init { container mkv }
        }"#;
        let ast = parse_policy(input).unwrap();
        let config = ast.config.unwrap();
        assert_eq!(config.audio_languages, vec!["eng", "und"]);
        assert_eq!(config.subtitle_languages, vec!["eng"]);
        assert_eq!(config.on_error.unwrap(), "continue");
        assert_eq!(config.commentary_patterns, vec!["commentary", "director"]);
    }

    #[test]
    fn test_parse_keep_with_filter() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang in [eng, jpn]
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0] {
            OperationNode::Keep { target, filter } => {
                assert_eq!(target, "audio");
                assert!(filter.is_some());
                match filter.as_ref().unwrap() {
                    FilterNode::LangIn(langs) => assert_eq!(langs, &["eng", "jpn"]),
                    _ => panic!("expected LangIn filter"),
                }
            }
            _ => panic!("expected Keep operation"),
        }
    }

    #[test]
    fn test_parse_transcode() {
        let input = r#"policy "test" {
            phase tc {
                transcode video to hevc {
                    crf: 20
                    preset: medium
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0] {
            OperationNode::Transcode {
                target,
                codec,
                settings,
            } => {
                assert_eq!(target, "video");
                assert_eq!(codec, "hevc");
                assert_eq!(settings.len(), 2);
            }
            _ => panic!("expected Transcode"),
        }
    }

    #[test]
    fn test_parse_when_block() {
        let input = r#"policy "test" {
            phase validate {
                when exists(audio where lang == jpn) {
                    warn "has japanese audio"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0] {
            OperationNode::When(when) => {
                assert_eq!(when.then_actions.len(), 1);
                match &when.then_actions[0] {
                    ActionNode::Warn(msg) => assert!(msg.contains("japanese")),
                    _ => panic!("expected Warn action"),
                }
            }
            _ => panic!("expected When"),
        }
    }
}
