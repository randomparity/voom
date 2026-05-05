//! `proptest` strategies for AST nodes. Strategies are intentionally bounded
//! in depth and width to keep test runtime tractable — the goal is to catch
//! parser/formatter drift, not to enumerate the entire grammar.
//!
//! Scope is deliberately narrow: only the operations `Container`, `Keep`,
//! `Remove`, `Order`, `Defaults`. Other `OperationNode` variants are not
//! generated yet — extend the strategies as new variants gain coverage.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::string::string_regex;

use crate::ast::{
    CompareOp, ConfigNode, FilterNode, OperationNode, PhaseNode, PolicyAst, Span, SpannedOperation,
};

/// Dummy span used for generated ASTs. The parser overwrites spans on the
/// roundtrip; the test strips spans before comparing.
fn dummy_span() -> Span {
    Span::new(0, 0, 1, 1)
}

fn ident_strategy() -> impl Strategy<Value = String> {
    string_regex("[a-z][a-z0-9_]{0,15}").expect("ident regex compiles")
}

fn policy_name_strategy() -> impl Strategy<Value = String> {
    // Match the grammar's string literal contents — printable ASCII without
    // `"` or `\`. Non-empty so we don't lose the field on reparse.
    string_regex("[A-Za-z0-9 _\\-]{1,32}").expect("policy name regex compiles")
}

fn language_strategy() -> impl Strategy<Value = String> {
    string_regex("[a-z]{2,3}").expect("language regex compiles")
}

fn codec_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("aac".to_string()),
        Just("ac3".to_string()),
        Just("dts".to_string()),
        Just("flac".to_string()),
        Just("h264".to_string()),
        Just("h265".to_string()),
        Just("opus".to_string()),
    ]
}

/// Numeric comparison operators — used for `channels` filters where the full
/// range of operators is accepted by the parser.
fn numeric_compare_op_strategy() -> impl Strategy<Value = CompareOp> {
    prop_oneof![
        Just(CompareOp::Eq),
        Just(CompareOp::Ne),
        Just(CompareOp::Lt),
        Just(CompareOp::Le),
        Just(CompareOp::Gt),
        Just(CompareOp::Ge),
    ]
}

fn track_target_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("audio".to_string()),
        Just("subtitle".to_string()),
        Just("subtitles".to_string()),
        Just("video".to_string()),
        Just("attachment".to_string()),
        Just("attachments".to_string()),
    ]
}

/// Atomic `FilterNode` leaves — no logical connectives. The parser rewrites
/// `lang == X` and `codec == X` into `LangIn`/`CodecIn` singleton lists, so
/// generating `Eq` for `LangCompare`/`CodecCompare` would not round-trip.
/// Only `Ne` survives as `LangCompare`/`CodecCompare`; other operators are
/// rejected by the parser entirely.
fn filter_leaf_strategy() -> impl Strategy<Value = FilterNode> {
    prop_oneof![
        vec(language_strategy(), 1..=3).prop_map(FilterNode::LangIn),
        vec(codec_strategy(), 1..=3).prop_map(FilterNode::CodecIn),
        language_strategy().prop_map(|l| FilterNode::LangCompare(CompareOp::Ne, l)),
        codec_strategy().prop_map(|c| FilterNode::CodecCompare(CompareOp::Ne, c)),
        (
            numeric_compare_op_strategy(),
            (1u32..=8).prop_map(f64::from)
        )
            .prop_map(|(op, ch)| FilterNode::Channels(op, ch)),
        Just(FilterNode::Commentary),
        Just(FilterNode::Forced),
        Just(FilterNode::Default),
    ]
}

/// Strategy for [`FilterNode`] expressions. Generated trees are normalized so
/// that the format/parse roundtrip preserves structure:
///
/// * `And` children are never `And` (parser flattens `A and (B and C)` to
///   `And([A, B, C])`).
/// * `Or` children are never `Or` (same flattening for `or`).
/// * `Not` children are never `Not` (`not not X` does not parse).
///
/// The strategy enforces these invariants by post-processing recursive
/// subtrees to wrap forbidden children in `Not(...)` (or unwrap a leading
/// `Not`).
pub fn filter_strategy() -> impl Strategy<Value = FilterNode> {
    filter_leaf_strategy().prop_recursive(3, 16, 3, |inner| {
        let and_child = inner.clone().prop_map(|f| match f {
            FilterNode::And(_) => FilterNode::Not(Box::new(f)),
            other => other,
        });
        let or_child = inner.clone().prop_map(|f| match f {
            FilterNode::Or(_) => FilterNode::Not(Box::new(f)),
            other => other,
        });
        let not_child = inner.prop_map(|f| match f {
            // `not not X` does not parse, so unwrap a leading Not.
            FilterNode::Not(inner) => *inner,
            other => other,
        });
        prop_oneof![
            vec(and_child, 2..=3).prop_map(FilterNode::And),
            vec(or_child, 2..=3).prop_map(FilterNode::Or),
            not_child.prop_map(|f| FilterNode::Not(Box::new(f))),
        ]
    })
}

/// Strategy for the focused subset of [`OperationNode`] currently covered.
pub fn operation_strategy() -> impl Strategy<Value = OperationNode> {
    prop_oneof![
        Just(OperationNode::Container("mkv".to_string())),
        Just(OperationNode::Container("mp4".to_string())),
        (
            track_target_strategy(),
            proptest::option::of(filter_strategy())
        )
            .prop_map(|(target, filter)| OperationNode::Keep { target, filter }),
        (
            track_target_strategy(),
            proptest::option::of(filter_strategy())
        )
            .prop_map(|(target, filter)| OperationNode::Remove { target, filter }),
        vec(language_strategy(), 1..=4).prop_map(OperationNode::Order),
        vec(
            (
                // Grammar accepts only "audio" or "subtitle" as the kind in
                // `default_item`, even though the parser normalizes
                // "subtitles" to "subtitle" elsewhere.
                prop_oneof![Just("audio".to_string()), Just("subtitle".to_string())],
                prop_oneof![
                    Just("first".to_string()),
                    Just("first_per_language".to_string()),
                    Just("none".to_string()),
                    Just("all".to_string()),
                ],
            ),
            1..=2,
        )
        .prop_map(OperationNode::Defaults),
    ]
}

fn spanned_op_strategy() -> impl Strategy<Value = SpannedOperation> {
    operation_strategy().prop_map(|node| SpannedOperation {
        node,
        span: dummy_span(),
    })
}

fn phase_strategy() -> impl Strategy<Value = PhaseNode> {
    (ident_strategy(), vec(spanned_op_strategy(), 0..=4)).prop_map(|(name, operations)| PhaseNode {
        name,
        skip_when: None,
        depends_on: Vec::new(),
        run_if: None,
        on_error: None,
        operations,
        span: dummy_span(),
    })
}

fn config_strategy() -> impl Strategy<Value = ConfigNode> {
    (
        vec(language_strategy(), 0..=3),
        vec(language_strategy(), 0..=3),
    )
        .prop_map(|(audio_languages, subtitle_languages)| ConfigNode {
            audio_languages,
            subtitle_languages,
            on_error: None,
            commentary_patterns: Vec::new(),
            keep_backups: None,
            span: dummy_span(),
        })
}

/// Top-level strategy that builds a complete [`PolicyAst`] for roundtrip tests.
pub fn policy_ast_strategy() -> impl Strategy<Value = PolicyAst> {
    (
        policy_name_strategy(),
        proptest::option::of(config_strategy()),
        vec(phase_strategy(), 1..=3),
    )
        .prop_map(|(name, config, phases)| PolicyAst {
            name,
            config,
            phases,
            span: dummy_span(),
        })
}
