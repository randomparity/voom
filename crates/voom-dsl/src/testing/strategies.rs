//! `proptest` strategies for AST nodes. Strategies are intentionally bounded
//! in depth and width to keep test runtime tractable — the goal is to catch
//! parser/formatter drift, not to enumerate the entire grammar.
//!
//! Coverage is grown in rounds (see GitHub issue #228). Currently covered:
//! `Container`, `Keep`, `Remove`, `Order`, `Defaults`, `When`, `Rules`,
//! `Transcode` for `OperationNode`; the filter leaves currently enumerated
//! in [`filter_leaf_strategy`] (full coverage in Round 3); and a minimal
//! `condition_leaf_strategy` /
//! `non_skip_action_strategy` (assembled by `action_vec_strategy`) used by
//! `When`/`Rules`.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::string::string_regex;

use crate::ast::{
    CompareOp, ConditionNode, ConfigNode, FilterNode, OperationNode, PhaseNode, PolicyAst, Span,
    SpannedOperation,
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

/// Short, parser-safe string used for `warn`/`fail` messages, synthesize
/// names, and `set_tag` keys. Avoids `"` and `\` (the only chars the string
/// escape pass touches) so format/parse is byte-identical.
fn safe_string_strategy() -> impl Strategy<Value = String> {
    string_regex("[A-Za-z0-9 _\\-]{1,16}").expect("safe string regex compiles")
}

/// Track-query target accepted inside `exists()` / `count()` conditions.
/// Currently delegates to [`track_target_strategy`]; the literal `"track"`
/// (any-kind) variant of the grammar's `track_query` rule is intentionally
/// excluded for now — that branch parses through a separate code path and
/// will be added when round 4 widens condition coverage.
fn track_query_target_strategy() -> impl Strategy<Value = String> {
    track_target_strategy()
}

/// Condition leaves used inside Round 1's `When`/`Rules`. This is the
/// minimal set; Round 4 wraps these in a recursive `condition_strategy`
/// with `And`/`Or`/`Not` and the `field_access` shapes.
fn condition_leaf_strategy() -> impl Strategy<Value = ConditionNode> {
    use crate::ast::TrackQueryNode;

    let exists = (
        track_query_target_strategy(),
        proptest::option::of(filter_strategy()),
    )
        .prop_map(|(target, filter)| ConditionNode::Exists(TrackQueryNode { target, filter }));

    let count = (
        track_query_target_strategy(),
        proptest::option::of(filter_strategy()),
        numeric_compare_op_strategy(),
        (0u32..=8).prop_map(f64::from),
    )
        .prop_map(|(target, filter, op, n)| {
            ConditionNode::Count(TrackQueryNode { target, filter }, op, n)
        });

    prop_oneof![
        Just(ConditionNode::AudioIsMultiLanguage),
        Just(ConditionNode::IsDubbed),
        Just(ConditionNode::IsOriginal),
        exists,
        count,
    ]
}

/// Non-`Skip` `ActionNode` strategy. `Skip` is excluded here because the
/// grammar rule `"skip" ~ ident?` is greedy: when followed by another
/// action, the next action's leading identifier (e.g. `keep`, `audio`) is
/// silently consumed as the optional skip phase. Use [`action_vec_strategy`]
/// to assemble lists; it appends an optional `Skip` only at the end.
fn non_skip_action_strategy() -> impl Strategy<Value = crate::ast::ActionNode> {
    use crate::ast::ActionNode;

    let keep = (
        track_target_strategy(),
        proptest::option::of(filter_strategy()),
    )
        .prop_map(|(target, filter)| ActionNode::Keep { target, filter });

    let remove = (
        track_target_strategy(),
        proptest::option::of(filter_strategy()),
    )
        .prop_map(|(target, filter)| ActionNode::Remove { target, filter });

    let warn = safe_string_strategy().prop_map(ActionNode::Warn);
    let fail = safe_string_strategy().prop_map(ActionNode::Fail);

    prop_oneof![keep, remove, warn, fail]
}

/// `Skip` action strategy — used as the only valid trailing action in a
/// `then_actions` / `else_actions` sequence (see [`action_vec_strategy`]).
fn skip_action_strategy() -> impl Strategy<Value = crate::ast::ActionNode> {
    use crate::ast::ActionNode;
    proptest::option::of(ident_strategy()).prop_map(ActionNode::Skip)
}

/// Build a sequence of actions of length within `min..=max`. A trailing
/// `Skip` is occasionally appended; it is never placed in a non-final
/// position because the grammar greedily attaches the next action's leading
/// identifier to the optional `skip <phase>` argument.
fn action_vec_strategy(
    min: usize,
    max: usize,
) -> impl Strategy<Value = Vec<crate::ast::ActionNode>> {
    debug_assert!(min <= max && max >= 1);

    (
        vec(non_skip_action_strategy(), min..=max),
        proptest::option::of(skip_action_strategy()),
    )
        .prop_map(move |(mut body, trailing_skip)| {
            if let Some(skip) = trailing_skip {
                if body.len() == max {
                    // Replace the last element instead of overflowing `max`.
                    body.pop();
                }
                body.push(skip);
            }
            body
        })
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
    use crate::ast::{RuleNode, Value, WhenNode};

    let container = prop_oneof![
        Just(OperationNode::Container("mkv".to_string())),
        Just(OperationNode::Container("mp4".to_string())),
    ];

    let keep = (
        track_target_strategy(),
        proptest::option::of(filter_strategy()),
    )
        .prop_map(|(target, filter)| OperationNode::Keep { target, filter });

    let remove = (
        track_target_strategy(),
        proptest::option::of(filter_strategy()),
    )
        .prop_map(|(target, filter)| OperationNode::Remove { target, filter });

    let order = vec(language_strategy(), 1..=4).prop_map(OperationNode::Order);

    let defaults = vec(
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
    .prop_map(OperationNode::Defaults);

    let when = (
        condition_leaf_strategy(),
        action_vec_strategy(1, 3),
        action_vec_strategy(0, 2),
    )
        .prop_map(|(condition, then_actions, else_actions)| {
            OperationNode::When(WhenNode {
                condition,
                then_actions,
                else_actions,
                span: dummy_span(),
            })
        });

    let rules = (
        prop_oneof![Just("first".to_string()), Just("all".to_string())],
        vec(
            (
                safe_string_strategy(),
                condition_leaf_strategy(),
                action_vec_strategy(1, 2),
            )
                .prop_map(|(name, condition, then_actions)| RuleNode {
                    name,
                    when: WhenNode {
                        condition,
                        then_actions,
                        else_actions: Vec::new(),
                        span: dummy_span(),
                    },
                }),
            1..=3,
        ),
    )
        .prop_map(|(mode, rules)| OperationNode::Rules { mode, rules });

    // Transcode kv settings: keys are valid identifiers from the
    // CompiledTranscodeSettings name-space; values are constrained to
    // shapes whose AST -> source -> AST roundtrip is byte-stable.
    let transcode_setting = prop_oneof![
        // Standard x264/x265 CRF range.
        (1u32..=51).prop_map(|n| (
            "crf".to_string(),
            Value::Number(f64::from(n), n.to_string())
        )),
        prop_oneof![
            Just("ultrafast"),
            Just("medium"),
            Just("slow"),
            Just("veryslow")
        ]
        .prop_map(|p| ("preset".to_string(), Value::Ident(p.to_string()))),
        prop_oneof![Just("128k"), Just("192k"), Just("256k"), Just("320k")]
            .prop_map(|b| ("bitrate".to_string(), Value::String(b.to_string()))),
        prop_oneof![Just("auto"), Just("nvenc"), Just("vaapi"), Just("none")]
            .prop_map(|h| ("hw".to_string(), Value::Ident(h.to_string()))),
    ];

    let transcode = (
        prop_oneof![Just("video".to_string()), Just("audio".to_string())],
        prop_oneof![
            Just("hevc".to_string()),
            Just("h264".to_string()),
            Just("aac".to_string()),
            Just("opus".to_string()),
        ],
        vec(transcode_setting, 0..=4),
    )
        .prop_map(|(target, codec, settings)| OperationNode::Transcode {
            target,
            codec,
            settings,
        });

    prop_oneof![container, keep, remove, order, defaults, when, rules, transcode]
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
