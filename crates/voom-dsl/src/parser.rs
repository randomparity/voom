//! Parser for the VOOM DSL.
//!
//! Uses pest to parse `.voom` source text into a CST, then converts
//! the CST into typed AST nodes defined in [`crate::ast`].
//!
//! The `.unwrap()` calls throughout this module are safe because pest guarantees
//! the CST structure matches the grammar — child nodes are always present when
//! the grammar says they must be.
#![allow(clippy::unwrap_used)]

use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

use crate::ast::*;
use crate::errors::{DslError, Result};

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct VoomParser;

/// Maximum allowed policy source size (1 MiB).
const MAX_POLICY_SIZE: usize = 1_024 * 1_024;

/// Maximum nesting depth for conditions and filters to prevent stack overflow.
const MAX_NESTING_DEPTH: usize = 100;

/// Parse a `.voom` source string into a [`PolicyAst`].
pub fn parse_policy(input: &str) -> Result<PolicyAst> {
    if input.len() > MAX_POLICY_SIZE {
        return Err(DslError::parse(
            1,
            1,
            format!(
                "policy too large: {} bytes (max {MAX_POLICY_SIZE})",
                input.len()
            ),
        ));
    }

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
            let values = build_list(list_pair);
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
            commentary_patterns = build_list(list_pair);
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
                depends_on = build_list(list);
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
                let op_span = span_from_pair(&child);
                let ident = child.into_inner().next().unwrap();
                operations.push(SpannedOperation {
                    span: op_span,
                    node: OperationNode::Container(ident.as_str().to_string()),
                });
            }
            Rule::keep_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_keep_remove(child, true)?,
                });
            }
            Rule::remove_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_keep_remove(child, false)?,
                });
            }
            Rule::order_op => {
                let op_span = span_from_pair(&child);
                let list = child.into_inner().next().unwrap();
                operations.push(SpannedOperation {
                    span: op_span,
                    node: OperationNode::Order(build_list(list)),
                });
            }
            Rule::defaults_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_defaults(child)?,
                });
            }
            Rule::actions_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_actions(child)?,
                });
            }
            Rule::transcode_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_transcode(child)?,
                });
            }
            Rule::synthesize_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_synthesize(child)?,
                });
            }
            Rule::clear_tags_op => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: OperationNode::ClearTags,
                });
            }
            Rule::set_tag_op => {
                let op_span = span_from_pair(&child);
                let mut tag_inner = child.into_inner();
                let tag = parse_string_value(&tag_inner.next().unwrap());
                let val = parse_set_tag_value(tag_inner.next().unwrap());
                operations.push(SpannedOperation {
                    span: op_span,
                    node: OperationNode::SetTag { tag, value: val },
                });
            }
            Rule::delete_tag_op => {
                let op_span = span_from_pair(&child);
                let tag = parse_string_value(&child.into_inner().next().unwrap());
                operations.push(SpannedOperation {
                    span: op_span,
                    node: OperationNode::DeleteTag(tag),
                });
            }
            Rule::when_block => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: OperationNode::When(build_when(child)?),
                });
            }
            Rule::rules_block => {
                let op_span = span_from_pair(&child);
                operations.push(SpannedOperation {
                    span: op_span,
                    node: build_rules(child)?,
                });
            }
            other => {
                let (line, col) = child.as_span().start_pos().line_col();
                return Err(DslError::build(
                    line,
                    col,
                    format!("unexpected rule {other:?}"),
                ));
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
    // Use the second word to determine target (grammar guarantees "video" or "audio")
    let target = text
        .split_whitespace()
        .nth(1)
        .unwrap_or("audio")
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
    build_condition_depth(pair, 0)
}

fn build_condition_depth(pair: Pair<'_, Rule>, depth: usize) -> Result<ConditionNode> {
    let inner = pair.into_inner().next().unwrap();
    build_condition_or(inner, depth)
}

fn build_condition_or(pair: Pair<'_, Rule>, depth: usize) -> Result<ConditionNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_condition_and(parts.into_iter().next().unwrap(), depth);
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_condition_and(p, depth)?);
    }
    Ok(ConditionNode::Or(nodes))
}

fn build_condition_and(pair: Pair<'_, Rule>, depth: usize) -> Result<ConditionNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_condition_not(parts.into_iter().next().unwrap(), depth);
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_condition_not(p, depth)?);
    }
    Ok(ConditionNode::And(nodes))
}

fn build_condition_not(pair: Pair<'_, Rule>, depth: usize) -> Result<ConditionNode> {
    let text = pair.as_str().trim();
    let inner = pair.into_inner().next().unwrap();
    if text.starts_with("not") {
        Ok(ConditionNode::Not(Box::new(build_condition_atom(
            inner, depth,
        )?)))
    } else {
        build_condition_atom(inner, depth)
    }
}

fn build_condition_atom(pair: Pair<'_, Rule>, depth: usize) -> Result<ConditionNode> {
    let text = pair.as_str().trim();
    let pair_span = pair.as_span();
    let (pair_line, pair_col) = pair_span.start_pos().line_col();
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
        let new_depth = depth + 1;
        if new_depth > MAX_NESTING_DEPTH {
            return Err(DslError::parse(
                pair_line,
                pair_col,
                format!("condition nesting depth exceeds maximum of {MAX_NESTING_DEPTH}"),
            ));
        }
        let cond = inner.next().unwrap();
        return build_condition_depth(cond, new_depth);
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
    let text = pair.as_str().trim();
    let mut inner = pair.into_inner();

    // The grammar rule is: (track_target | "track") ~ ("where" ~ filter_expr)?
    // "track" is a string literal, so pest silently consumes it (no named child).
    // We detect this from the text and handle it specially.
    let (target, first_child) = if text.starts_with("track ") || text == "track" {
        // "track" was the literal — no named child for the target
        ("track".to_string(), inner.next())
    } else {
        let target_pair = inner.next().unwrap();
        (target_pair.as_str().to_string(), inner.next())
    };

    let filter = if let Some(pair) = first_child {
        if pair.as_rule() == Rule::filter_expr {
            Some(build_filter(pair)?)
        } else {
            None
        }
    } else {
        None
    };

    Ok(TrackQueryNode { target, filter })
}

fn build_filter(pair: Pair<'_, Rule>) -> Result<FilterNode> {
    build_filter_depth(pair, 0)
}

fn build_filter_depth(pair: Pair<'_, Rule>, depth: usize) -> Result<FilterNode> {
    let inner = pair.into_inner().next().unwrap();
    build_filter_or(inner, depth)
}

fn build_filter_or(pair: Pair<'_, Rule>, depth: usize) -> Result<FilterNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_filter_and(parts.into_iter().next().unwrap(), depth);
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_filter_and(p, depth)?);
    }
    Ok(FilterNode::Or(nodes))
}

fn build_filter_and(pair: Pair<'_, Rule>, depth: usize) -> Result<FilterNode> {
    let parts: Vec<_> = pair.into_inner().collect();
    if parts.len() == 1 {
        return build_filter_not(parts.into_iter().next().unwrap(), depth);
    }
    let mut nodes = Vec::new();
    for p in parts {
        nodes.push(build_filter_not(p, depth)?);
    }
    Ok(FilterNode::And(nodes))
}

fn build_filter_not(pair: Pair<'_, Rule>, depth: usize) -> Result<FilterNode> {
    let text = pair.as_str().trim();
    let inner = pair.into_inner().next().unwrap();
    if text.starts_with("not") {
        Ok(FilterNode::Not(Box::new(build_filter_atom(inner, depth)?)))
    } else {
        build_filter_atom(inner, depth)
    }
}

/// Result of parsing a `lang`/`codec` filter atom: either an `in` list or a comparison.
enum ListOrCompare {
    InList(Vec<String>),
    Compare(CompareOp, String),
}

/// Shared logic for `lang` and `codec` filter atoms, which have identical grammar structure:
/// `keyword (list | compare_op value)`.
fn build_list_or_compare_filter(
    inner: &mut pest::iterators::Pairs<'_, Rule>,
    span: pest::Span<'_>,
    kind: &str,
) -> Result<ListOrCompare> {
    let next = inner.next().unwrap();
    if next.as_rule() == Rule::list {
        return Ok(ListOrCompare::InList(build_list(next)));
    }
    let op = build_compare_op(next);
    let val = inner.next().unwrap();
    let val_str = match val.as_rule() {
        Rule::value => val.into_inner().next().unwrap().as_str().to_string(),
        _ => val.as_str().to_string(),
    };
    match op {
        CompareOp::Eq | CompareOp::In => Ok(ListOrCompare::InList(vec![val_str])),
        CompareOp::Ne => Ok(ListOrCompare::Compare(CompareOp::Ne, val_str)),
        _ => {
            let (line, col) = span.start_pos().line_col();
            Err(DslError::build(
                line,
                col,
                format!("operator {op:?} is not valid for {kind} comparisons; use == or !="),
            ))
        }
    }
}

fn build_filter_atom(pair: Pair<'_, Rule>, depth: usize) -> Result<FilterNode> {
    let text = pair.as_str().trim();
    let span = pair.as_span();
    let mut inner = pair.into_inner();

    if text.starts_with("lang") {
        return match build_list_or_compare_filter(&mut inner, span, "lang")? {
            ListOrCompare::InList(v) => Ok(FilterNode::LangIn(v)),
            ListOrCompare::Compare(op, v) => Ok(FilterNode::LangCompare(op, v)),
        };
    }
    if text.starts_with("codec") {
        return match build_list_or_compare_filter(&mut inner, span, "codec")? {
            ListOrCompare::InList(v) => Ok(FilterNode::CodecIn(v)),
            ListOrCompare::Compare(op, v) => Ok(FilterNode::CodecCompare(op, v)),
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
        let new_depth = depth + 1;
        if new_depth > MAX_NESTING_DEPTH {
            let (line, col) = span.start_pos().line_col();
            return Err(DslError::parse(
                line,
                col,
                format!("filter nesting depth exceeds maximum of {MAX_NESTING_DEPTH}"),
            ));
        }
        let filter = inner.next().unwrap();
        return build_filter_depth(filter, new_depth);
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
        let val = parse_set_tag_value(inner.next().unwrap());
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

/// Parse a `set_tag` value: either a field access or a literal value.
fn parse_set_tag_value(val_pair: Pair<'_, Rule>) -> ValueOrField {
    if val_pair.as_rule() == Rule::field_access {
        ValueOrField::Field(build_field_access(&val_pair))
    } else {
        ValueOrField::Value(build_value(val_pair))
    }
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
        _ => unreachable!("grammar only permits valid compare_op tokens"),
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
        Rule::list => Value::List(build_list_values(pair)),
        _ => Value::Ident(pair.as_str().to_string()),
    }
}

fn build_list(pair: Pair<'_, Rule>) -> Vec<String> {
    pair.into_inner()
        .map(|v| {
            let inner = v.into_inner().next().unwrap();
            match inner.as_rule() {
                Rule::string => parse_string_value(&inner),
                _ => inner.as_str().to_string(),
            }
        })
        .collect()
}

fn build_list_values(pair: Pair<'_, Rule>) -> Vec<Value> {
    pair.into_inner().map(build_value).collect()
}

/// Strip surrounding quotes from a string literal and process escape sequences.
fn parse_string_value(pair: &Pair<'_, Rule>) -> String {
    let s = pair.as_str();
    if s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut result = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => result.push('"'),
                    Some('\\') => result.push('\\'),
                    Some(other) => {
                        result.push('\\');
                        result.push(other);
                    }
                    None => result.push('\\'),
                }
            } else {
                result.push(c);
            }
        }
        result
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
        match &ast.phases[0].operations[0].node {
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
        match &ast.phases[0].operations[0].node {
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
    fn test_parse_rejects_oversized_input() {
        let input = "x".repeat(MAX_POLICY_SIZE + 1);
        let err = parse_policy(&input).unwrap_err();
        match err {
            DslError::Parse {
                line, col, message, ..
            } => {
                assert_eq!(line, 1);
                assert_eq!(col, 1);
                assert!(message.contains("policy too large"));
            }
            _ => panic!("expected Parse error for oversized input"),
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
        match &ast.phases[0].operations[0].node {
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

    #[test]
    fn test_lang_ne_parses_to_lang_compare() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang != jpn
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::Keep { filter, .. } => match filter.as_ref().unwrap() {
                FilterNode::LangCompare(CompareOp::Ne, lang) => assert_eq!(lang, "jpn"),
                other => panic!("expected LangCompare(Ne, jpn), got {other:?}"),
            },
            _ => panic!("expected Keep operation"),
        }
    }

    #[test]
    fn test_lang_eq_parses_to_lang_in() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang == jpn
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::Keep { filter, .. } => match filter.as_ref().unwrap() {
                FilterNode::LangIn(langs) => assert_eq!(langs, &["jpn"]),
                other => panic!("expected LangIn([jpn]), got {other:?}"),
            },
            _ => panic!("expected Keep operation"),
        }
    }

    #[test]
    fn test_lang_gt_returns_error() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where lang > jpn
            }
        }"#;
        let err = parse_policy(input).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not valid for lang comparisons"), "got: {msg}");
    }

    #[test]
    fn test_codec_ne_parses_to_codec_compare() {
        let input = r#"policy "test" {
            phase norm {
                keep audio where codec != aac
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::Keep { filter, .. } => match filter.as_ref().unwrap() {
                FilterNode::CodecCompare(CompareOp::Ne, codec) => assert_eq!(codec, "aac"),
                other => panic!("expected CodecCompare(Ne, aac), got {other:?}"),
            },
            _ => panic!("expected Keep operation"),
        }
    }

    #[test]
    fn test_transcode_video_codec_with_video_in_name() {
        // Ensure a codec name containing "video" doesn't confuse target detection
        let input = r#"policy "test" {
            phase tc {
                transcode audio to libvideo_codec {
                    bitrate: 192k
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::Transcode { target, codec, .. } => {
                assert_eq!(target, "audio");
                assert_eq!(codec, "libvideo_codec");
            }
            _ => panic!("expected Transcode"),
        }
    }

    #[test]
    fn test_deeply_nested_condition_rejected() {
        // Build deeply nested conditions using parenthesized sub-expressions.
        // Pest's recursive descent grammar may also have limits, so any error is acceptable.
        let mut cond = "is_dubbed".to_string();
        for _ in 0..=MAX_NESTING_DEPTH {
            cond = format!("({cond})");
        }
        let input = format!(
            r#"policy "test" {{
                phase validate {{
                    when {cond} {{
                        warn "deep"
                    }}
                }}
            }}"#
        );
        // Should fail either from our depth limit or pest's internal limits
        assert!(parse_policy(&input).is_err());
    }

    #[test]
    fn test_moderate_nesting_succeeds() {
        // A few levels of parenthesized nesting should work fine
        let mut cond = "is_dubbed".to_string();
        for _ in 0..5 {
            cond = format!("({cond})");
        }
        let input = format!(
            r#"policy "test" {{
                phase validate {{
                    when {cond} {{
                        warn "ok"
                    }}
                }}
            }}"#
        );
        assert!(parse_policy(&input).is_ok(), "failed to parse: {}", input);
    }

    #[test]
    fn test_string_escape_sequences() {
        let input = r#"policy "test" {
            phase validate {
                when is_dubbed {
                    warn "contains \"quoted\" text"
                }
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::When(when) => match &when.then_actions[0] {
                ActionNode::Warn(msg) => assert_eq!(msg, r#"contains "quoted" text"#),
                _ => panic!("expected Warn"),
            },
            _ => panic!("expected When"),
        }
    }

    #[test]
    fn test_spanned_operation_has_correct_span() {
        let input = r#"policy "test" {
            phase init {
                container mkv
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        let spanned = &ast.phases[0].operations[0];
        assert!(spanned.span.line > 0);
        assert!(spanned.span.col > 0);
        match &spanned.node {
            OperationNode::Container(name) => assert_eq!(name, "mkv"),
            _ => panic!("expected Container"),
        }
    }

    #[test]
    fn test_parse_clear_tags() {
        let input = r#"policy "test" {
            phase clean {
                clear_tags
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::ClearTags => {}
            other => panic!("expected ClearTags, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_set_tag_literal() {
        let input = r#"policy "test" {
            phase clean {
                set_tag "title" "My Movie"
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::SetTag { tag, value } => {
                assert_eq!(tag, "title");
                match value {
                    ValueOrField::Value(Value::String(s)) => assert_eq!(s, "My Movie"),
                    other => panic!("expected string value, got {other:?}"),
                }
            }
            other => panic!("expected SetTag, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_set_tag_field_access() {
        let input = r#"policy "test" {
            phase clean {
                set_tag "source" plugin.metadata
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::SetTag { tag, value } => {
                assert_eq!(tag, "source");
                match value {
                    ValueOrField::Field(path) => assert_eq!(path, &["plugin", "metadata"]),
                    other => panic!("expected field access, got {other:?}"),
                }
            }
            other => panic!("expected SetTag, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_delete_tag() {
        let input = r#"policy "test" {
            phase clean {
                delete_tag "encoder"
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        match &ast.phases[0].operations[0].node {
            OperationNode::DeleteTag(tag) => assert_eq!(tag, "encoder"),
            other => panic!("expected DeleteTag, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_combined_container_metadata() {
        let input = r#"policy "test" {
            phase clean {
                clear_tags
                container mkv
                set_tag "title" "My Movie"
                delete_tag "encoder"
            }
        }"#;
        let ast = parse_policy(input).unwrap();
        assert_eq!(ast.phases[0].operations.len(), 4);
        assert!(matches!(
            &ast.phases[0].operations[0].node,
            OperationNode::ClearTags
        ));
        assert!(matches!(
            &ast.phases[0].operations[1].node,
            OperationNode::Container(_)
        ));
        assert!(matches!(
            &ast.phases[0].operations[2].node,
            OperationNode::SetTag { .. }
        ));
        assert!(matches!(
            &ast.phases[0].operations[3].node,
            OperationNode::DeleteTag(_)
        ));
    }
}
