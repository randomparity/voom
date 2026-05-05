//! Property-based invariants for the policy evaluator: sort stability,
//! dedup idempotence, and predicate algebra (double-negation, De Morgan).

use std::collections::HashMap;
use std::path::PathBuf;

use proptest::collection::vec;
use proptest::prelude::*;

use voom_domain::media::{MediaFile, Track, TrackType};
use voom_domain::plan::{OperationType, Plan};
use voom_dsl::compile_policy;
use voom_policy_evaluator::evaluate;

/// Strategy for an audio Track: (language, codec, channels) tuple.
fn audio_track_strategy() -> impl Strategy<Value = (String, String, u32)> {
    (
        prop_oneof![
            Just("eng"),
            Just("jpn"),
            Just("fre"),
            Just("spa"),
            Just("ger")
        ]
        .prop_map(str::to_string),
        prop_oneof![
            Just("aac"),
            Just("ac3"),
            Just("dts"),
            Just("flac"),
            Just("opus")
        ]
        .prop_map(str::to_string),
        prop_oneof![Just(2u32), Just(6), Just(8)],
    )
}

/// Build a `MediaFile` with one fixed video track at index 0 and the given
/// audio tracks at sequential indices starting at 1.
fn build_file(audio: &[(String, String, u32)]) -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from("/test/movie.mkv"));

    let mut video = Track::new(0, TrackType::Video, "h264".into());
    video.language = "und".into();
    file.tracks.push(video);

    for (i, (lang, codec, channels)) in audio.iter().enumerate() {
        let idx = u32::try_from(i + 1).expect("audio count fits in u32");
        let mut t = Track::new(idx, TrackType::AudioMain, codec.clone());
        t.language = lang.clone();
        t.channels = Some(*channels);
        file.tracks.push(t);
    }
    file
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Sort stability: when `keep audio where lang in [...]` selects a subset
    /// of tracks, the resulting Plan emits actions referencing tracks in
    /// monotonically-increasing index order. Two tracks A and B that both
    /// pass the filter must appear in the plan in the same relative order
    /// as in the input file.
    #[test]
    fn keep_audio_preserves_track_order(audio in vec(audio_track_strategy(), 1..=8)) {
        let file = build_file(&audio);
        let policy = compile_policy(
            r#"policy "test" { phase init { keep audio where lang in [eng, jpn, fre] } }"#,
        ).unwrap();

        let result = evaluate(&policy, &file);
        prop_assert_eq!(result.plans.len(), 1);

        let touched: Vec<u32> = result.plans[0]
            .actions
            .iter()
            .filter_map(|a| a.track_index)
            .collect();

        // Strictly increasing indices ⇔ original file order preserved, because
        // file indices are assigned sequentially in insertion order.
        for w in touched.windows(2) {
            prop_assert!(w[0] < w[1], "Plan emitted indices out of order: {touched:?}");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Dedup idempotence: applying `defaults audio first_per_language` and
    /// then mutating the file in place to reflect those default-flag changes
    /// must leave no further work for a second evaluation. Equivalently:
    /// `defaults first_per_language` is a fixpoint operator on the
    /// `is_default` flag set.
    #[test]
    fn defaults_first_per_language_is_idempotent(
        audio in vec(audio_track_strategy(), 1..=6),
    ) {
        let file = build_file(&audio);
        let policy = compile_policy(
            r#"policy "test" { phase init { defaults { audio: first_per_language } } }"#,
        ).unwrap();

        let first = evaluate(&policy, &file);
        prop_assert_eq!(first.plans.len(), 1);

        let mut updated = file.clone();
        apply_default_actions(&mut updated, &first.plans[0]);

        // Asserted alongside the idempotence check below: a short-circuiting
        // evaluator could emit zero second-pass actions without actually
        // reaching the `first_per_language` fixpoint, so we witness the
        // end-state directly here.
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for t in &updated.tracks {
            if t.track_type == TrackType::AudioMain && t.is_default {
                *counts.entry(t.language.as_str()).or_default() += 1;
            }
        }
        for (lang, n) in &counts {
            prop_assert_eq!(*n, 1, "lang {} has {} default audio tracks after pass 1", lang, n);
        }

        let second = evaluate(&policy, &updated);
        prop_assert_eq!(second.plans.len(), 1);
        prop_assert_eq!(
            second.plans[0].actions.len(),
            0,
            "second pass emitted actions: {:?}",
            second.plans[0].actions,
        );
    }
}

/// Apply `SetDefault` / `ClearDefault` actions from a Plan to a MediaFile,
/// mirroring what an executor would do at run-time. Lives in this test file
/// because no production code performs this in-place mutation.
fn apply_default_actions(file: &mut MediaFile, plan: &Plan) {
    for action in &plan.actions {
        let Some(idx) = action.track_index else {
            continue;
        };
        let Some(track) = file.tracks.iter_mut().find(|t| t.index == idx) else {
            continue;
        };
        match action.operation {
            OperationType::SetDefault => track.is_default = true,
            OperationType::ClearDefault => track.is_default = false,
            other => panic!(
                "apply_default_actions only models SetDefault/ClearDefault; got {other:?} \
                 — extend the helper or narrow the policy"
            ),
        }
    }
}

/// Build a `skip when <condition>` policy and return whether the skip fires
/// (i.e., whether the condition evaluates to true) when applied to `file`.
fn skip_fires(file: &MediaFile, condition_dsl: &str) -> bool {
    let src =
        format!("policy \"p\" {{ phase init {{ skip when {condition_dsl} container mkv }} }}");
    let policy = compile_policy(&src).unwrap_or_else(|e| panic!("dsl: {src}\nerr: {e:?}"));
    let result = evaluate(&policy, file);
    result.plans[0].is_skipped()
}

/// Strategy for a grammar-valid DSL `condition_atom` string.
///
/// Covers the shapes called out in issue #229:
/// - plain atoms: `is_dubbed`, `is_original`, `audio_is_multi_language`
/// - `exists(<target> [where <filter_atom>])`
/// - `count(<target> [where <filter_atom>]) <op> <number>`
/// - `<field_access> <op> <value>`
/// - `<field_access> exists`
///
/// Excludes `in` from comparison ops in field-compare and count variants
/// (it requires a list RHS, which is handled by dedicated list-shaped
/// variants in the filter sub-strategy). Field paths are restricted to
/// `audio.*` and `video.*`, which the evaluator's `resolve_field` knows
/// how to look up; unresolvable paths still produce deterministic
/// `false`, but keeping the strategy on resolvable paths exercises more
/// of the evaluator.
fn condition_dsl_strategy() -> impl Strategy<Value = String> {
    // Scalars used inside generated strings.
    let lang = prop_oneof![
        Just("eng"),
        Just("jpn"),
        Just("fre"),
        Just("spa"),
        Just("ger"),
    ];
    let codec = prop_oneof![
        Just("aac"),
        Just("ac3"),
        Just("dts"),
        Just("flac"),
        Just("opus"),
    ];
    let target = prop_oneof![Just("audio"), Just("subtitle"), Just("video")];
    // Comparison ops that take scalar RHS (no `in`).
    let scalar_cmp = prop_oneof![
        Just("=="),
        Just("!="),
        Just("<"),
        Just("<="),
        Just(">"),
        Just(">="),
    ];
    let small_num = 0u32..=8u32;

    // ----- filter_atom variants used inside `where` clauses -----

    let lang_in_list = lang.clone().prop_map(|l| format!("lang in [{l}]"));
    let codec_in_list = codec.clone().prop_map(|c| format!("codec in [{c}]"));
    let channels_cmp =
        (scalar_cmp.clone(), small_num.clone()).prop_map(|(op, n)| format!("channels {op} {n}"));
    let bare_filter = prop_oneof![
        Just("commentary".to_string()),
        Just("forced".to_string()),
        Just("default".to_string()),
    ];
    let filter_atom = prop_oneof![lang_in_list, codec_in_list, channels_cmp, bare_filter];

    // Optional ` where <filter_atom>` suffix (or empty).
    let where_clause = proptest::option::of(filter_atom)
        .prop_map(|f| f.map_or(String::new(), |s| format!(" where {s}")));

    // ----- condition_atom variants -----

    let plain = prop_oneof![
        Just("is_dubbed".to_string()),
        Just("is_original".to_string()),
        Just("audio_is_multi_language".to_string()),
    ];

    let exists_atom =
        (target.clone(), where_clause.clone()).prop_map(|(t, w)| format!("exists({t}{w})"));

    let count_atom = (
        target.clone(),
        where_clause.clone(),
        scalar_cmp.clone(),
        small_num.clone(),
    )
        .prop_map(|(t, w, op, n)| format!("count({t}{w}) {op} {n}"));

    // Field-access compare with a string RHS (codec/language fields).
    let field_string_cmp = (
        prop_oneof![
            Just("audio.codec"),
            Just("audio.language"),
            Just("audio.title"),
        ],
        scalar_cmp.clone(),
        prop_oneof![
            codec.clone().prop_map(|c| format!("\"{c}\"")),
            lang.clone().prop_map(|l| format!("\"{l}\"")),
        ],
    )
        .prop_map(|(f, op, v)| format!("{f} {op} {v}"));

    // Field-access compare with a numeric RHS (channels/width/height).
    let field_num_cmp = (
        prop_oneof![
            Just("audio.channels"),
            Just("video.width"),
            Just("video.height"),
        ],
        scalar_cmp,
        small_num,
    )
        .prop_map(|(f, op, n)| format!("{f} {op} {n}"));

    let field_exists = prop_oneof![
        Just("audio.codec exists".to_string()),
        Just("audio.language exists".to_string()),
        Just("video.codec exists".to_string()),
    ];

    prop_oneof![
        plain,
        exists_atom,
        count_atom,
        field_string_cmp,
        field_num_cmp,
        field_exists,
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every string emitted by `condition_dsl_strategy` must compile as a
    /// `skip when <p>` policy. This catches grammar-violating shapes in
    /// the strategy itself before the algebra tests would notice.
    #[test]
    fn condition_dsl_strategy_emits_compilable_predicates(
        p in condition_dsl_strategy(),
    ) {
        let src = format!("policy \"p\" {{ phase init {{ skip when {p} container mkv }} }}");
        let res = compile_policy(&src);
        prop_assert!(
            res.is_ok(),
            "strategy emitted non-compilable predicate `{p}`: {:?}",
            res.err(),
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Identity: `not (not P) ≡ P` for any predicate P drawn from
    /// `condition_dsl_strategy`.
    #[test]
    fn double_negation_identity(
        audio in vec(audio_track_strategy(), 0..=4),
        p in condition_dsl_strategy(),
    ) {
        let file = build_file(&audio);
        let direct = skip_fires(&file, &p);
        let double_neg = skip_fires(&file, &format!("not (not ({p}))"));
        // Positional arg: prop_assert_eq! routes through concat!, which rejects {capture}.
        prop_assert_eq!(
            direct, double_neg,
            "double-negation broke for predicate `{}`",
            p,
        );
    }

    /// De Morgan's law: `not (A and B) ≡ (not A) or (not B)`.
    #[test]
    fn de_morgan_and(audio in vec(audio_track_strategy(), 0..=4)) {
        let file = build_file(&audio);
        let a = "exists(audio where lang in [eng])";
        let b = "exists(audio where codec in [aac])";
        let lhs = skip_fires(&file, &format!("not (({a}) and ({b}))"));
        let rhs = skip_fires(&file, &format!("(not ({a})) or (not ({b}))"));
        prop_assert_eq!(lhs, rhs);
    }

    /// De Morgan's law: `not (A or B) ≡ (not A) and (not B)`.
    #[test]
    fn de_morgan_or(audio in vec(audio_track_strategy(), 0..=4)) {
        let file = build_file(&audio);
        let a = "exists(audio where lang in [eng])";
        let b = "exists(audio where codec in [aac])";
        let lhs = skip_fires(&file, &format!("not (({a}) or ({b}))"));
        let rhs = skip_fires(&file, &format!("(not ({a})) and (not ({b}))"));
        prop_assert_eq!(lhs, rhs);
    }
}
