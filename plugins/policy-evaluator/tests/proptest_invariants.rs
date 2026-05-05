//! Property-based invariants for the policy evaluator: sort stability,
//! dedup idempotence, and predicate algebra (double-negation, De Morgan).

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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Identity: `not (not P) ≡ P`.
    #[test]
    fn double_negation_identity(audio in vec(audio_track_strategy(), 0..=4)) {
        let file = build_file(&audio);
        let p = "exists(audio where lang in [eng])";
        let direct = skip_fires(&file, p);
        let double_neg = skip_fires(&file, &format!("not (not ({p}))"));
        prop_assert_eq!(direct, double_neg);
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
