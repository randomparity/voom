//! Property-based invariants for the policy evaluator. See issue #216.

use std::path::PathBuf;

use proptest::collection::vec;
use proptest::prelude::*;

use voom_domain::media::{MediaFile, Track, TrackType};
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
    let mut next_index = 0u32;

    let mut video = Track::new(next_index, TrackType::Video, "h264".into());
    video.language = "und".into();
    file.tracks.push(video);
    next_index += 1;

    for (lang, codec, channels) in audio {
        let mut t = Track::new(next_index, TrackType::AudioMain, codec.clone());
        t.language = lang.clone();
        t.channels = Some(*channels);
        file.tracks.push(t);
        next_index += 1;
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
        // Phase 'init' produces exactly one Plan.
        prop_assert_eq!(result.plans.len(), 1);

        // Extract the track indices the plan touches, in emission order.
        let touched: Vec<u32> = result.plans[0]
            .actions
            .iter()
            .filter_map(|a| a.track_index)
            .collect();

        // The indices must be strictly increasing — equivalent to "preserves
        // original file order" since file indices are assigned sequentially
        // in insertion order.
        for w in touched.windows(2) {
            prop_assert!(w[0] < w[1], "Plan emitted indices out of order: {touched:?}");
        }
    }
}
