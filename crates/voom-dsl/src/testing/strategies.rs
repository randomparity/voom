//! `proptest` strategies for AST nodes. Strategies are intentionally bounded
//! in depth and width to keep test runtime tractable — the goal is to catch
//! parser/formatter drift, not to enumerate the entire grammar.
//!
//! Coverage is grown in rounds (see GitHub issue #228). Currently covered:
//! every `OperationNode` variant except `SynthSetting::CreateIf` and the
//! phase-level `skip_when` / `run_if` / `when` shapes (Round 4); the filter
//! leaves currently enumerated in [`filter_leaf_strategy`] (full coverage
//! in Round 3); and a minimal `condition_leaf_strategy` /
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

/// `Value` strategy whose AST -> source -> AST roundtrip is byte-stable.
/// `Value::Number` is restricted to small non-negative integers whose raw
/// text equals the digit form printed by the formatter. Idents that would
/// re-parse as another `Value` shape (`true`/`false` re-parse as
/// `Value::Bool`) are excluded.
fn value_strategy() -> impl Strategy<Value = crate::ast::Value> {
    use crate::ast::Value;

    let int_number = (0u32..=4096).prop_map(|n| Value::Number(f64::from(n), n.to_string()));

    // The grammar's `value` rule tries `boolean` before `ident`, so an
    // ident literally spelled `true` or `false` would round-trip as a
    // `Value::Bool` instead of a `Value::Ident`.
    let safe_ident = ident_strategy().prop_filter("ident must not collide with boolean", |s| {
        s != "true" && s != "false"
    });

    prop_oneof![
        safe_string_strategy().prop_map(Value::String),
        int_number,
        prop_oneof![Just(true), Just(false)].prop_map(Value::Bool),
        safe_ident.prop_map(Value::Ident),
    ]
}

/// `ValueOrField` strategy used by `set_tag` (action and operation).
/// `Field` paths must contain >=2 segments to satisfy the
/// `field_access = ident ~ ("." ~ ident)+` grammar rule.
fn value_or_field_strategy() -> impl Strategy<Value = crate::ast::ValueOrField> {
    use crate::ast::ValueOrField;

    let field = vec(ident_strategy(), 2..=3).prop_map(ValueOrField::Field);
    let value = value_strategy().prop_map(ValueOrField::Value);
    prop_oneof![field, value]
}

/// `ValueOrField` narrowed to the `(field_access | string)` arm.
/// Used by `set_language`, whose grammar is
/// `set_language ~ track_ref ~ (field_access | string)` — the parser
/// materialises the literal arm as `ValueOrField::Value(Value::String(_))`,
/// so generating `Value::Number`/`Bool`/`Ident` here would not round-trip.
fn field_or_string_strategy() -> impl Strategy<Value = crate::ast::ValueOrField> {
    use crate::ast::{Value, ValueOrField};

    let field = vec(ident_strategy(), 2..=3).prop_map(ValueOrField::Field);
    let string_value = safe_string_strategy().prop_map(|s| ValueOrField::Value(Value::String(s)));
    prop_oneof![field, string_value]
}

/// `action_setting` strategy used inside `actions { ... }` blocks. Keys
/// are identifiers (matching the `action_setting = ident ~ ":" ~ value`
/// rule). Structurally identical to `kv_pair` used inside `transcode`
/// `block`s.
fn action_setting_strategy() -> impl Strategy<Value = (String, crate::ast::Value)> {
    (ident_strategy(), value_strategy())
}

/// `SynthSetting` strategy. `CreateIf` is intentionally excluded — it
/// requires the full `condition_strategy` introduced in Round 4 and is
/// added to this `prop_oneof!` at that point.
fn synth_setting_strategy() -> impl Strategy<Value = crate::ast::SynthSetting> {
    use crate::ast::{SynthSetting, Value};

    let codec = prop_oneof![
        Just("aac".to_string()),
        Just("ac3".to_string()),
        Just("opus".to_string()),
    ]
    .prop_map(SynthSetting::Codec);

    let channels = prop_oneof![
        (1u32..=8).prop_map(|n| SynthSetting::Channels(Value::Number(f64::from(n), n.to_string()))),
        Just(SynthSetting::Channels(Value::Ident("stereo".to_string()))),
        Just(SynthSetting::Channels(Value::Ident("surround".to_string()))),
    ];

    let source = filter_strategy().prop_map(SynthSetting::Source);
    let bitrate = prop_oneof![Just("128k"), Just("192k"), Just("256k")]
        .prop_map(|b| SynthSetting::Bitrate(b.to_string()));
    let skip_if_exists = filter_strategy().prop_map(SynthSetting::SkipIfExists);
    let title = safe_string_strategy().prop_map(SynthSetting::Title);
    let language = prop_oneof![
        language_strategy().prop_map(SynthSetting::Language),
        Just(SynthSetting::Language("inherit".to_string())),
    ];
    let position = prop_oneof![
        Just(SynthSetting::Position(Value::Ident("first".to_string()))),
        Just(SynthSetting::Position(Value::Ident("last".to_string()))),
        (0u32..=16)
            .prop_map(|n| SynthSetting::Position(Value::Number(f64::from(n), n.to_string()))),
    ];

    prop_oneof![
        codec,
        channels,
        source,
        bitrate,
        skip_if_exists,
        title,
        language,
        position,
    ]
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

fn track_ref_strategy() -> impl Strategy<Value = crate::ast::TrackRefNode> {
    use crate::ast::TrackRefNode;

    (
        track_target_strategy(),
        proptest::option::of(filter_strategy()),
    )
        .prop_map(|(target, filter)| TrackRefNode { target, filter })
}

/// Non-`Skip` `ActionNode` strategy. `Skip` is excluded here because the
/// grammar rule `"skip" ~ ident?` is greedy: when followed by another
/// action, the next action's leading identifier (e.g. `keep`, `audio`) is
/// silently consumed as the optional skip phase. Use [`action_vec_strategy`]
/// to assemble lists; it appends an optional `Skip` only at the end via
/// [`skip_action_strategy`].
///
/// All Round 2 additions (`SetDefault`, `SetForced`, `SetLanguage`,
/// `SetTag`) have required tail tokens (a `track_ref`, a value/field, or a
/// string + value) so they have no greedy-grammar hazard and remain in the
/// non-skip pool.
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

    let set_default = track_ref_strategy().prop_map(ActionNode::SetDefault);
    let set_forced = track_ref_strategy().prop_map(ActionNode::SetForced);
    // `set_language` accepts only `field_access | string` per the grammar;
    // the parser materialises the literal arm as `Value::String(...)`, so
    // we restrict the value strategy accordingly.
    let set_language = (track_ref_strategy(), field_or_string_strategy())
        .prop_map(|(track, val)| ActionNode::SetLanguage(track, val));
    let set_tag = (safe_string_strategy(), value_or_field_strategy())
        .prop_map(|(tag, val)| ActionNode::SetTag(tag, val));

    prop_oneof![
        keep,
        remove,
        warn,
        fail,
        set_default,
        set_forced,
        set_language,
        set_tag,
    ]
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

    let actions_op = (
        prop_oneof![
            Just("audio".to_string()),
            Just("subtitle".to_string()),
            Just("video".to_string()),
        ],
        vec(action_setting_strategy(), 0..=3),
    )
        .prop_map(|(target, settings)| OperationNode::Actions { target, settings });

    let clear_tags = Just(OperationNode::ClearTags);

    let set_tag = (safe_string_strategy(), value_or_field_strategy())
        .prop_map(|(tag, value)| OperationNode::SetTag { tag, value });

    let delete_tag = safe_string_strategy().prop_map(OperationNode::DeleteTag);

    let synthesize = (safe_string_strategy(), vec(synth_setting_strategy(), 1..=4))
        .prop_map(|(name, settings)| OperationNode::Synthesize { name, settings });

    prop_oneof![
        container, keep, remove, order, defaults, when, rules, transcode, actions_op, clear_tags,
        set_tag, delete_tag, synthesize,
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
