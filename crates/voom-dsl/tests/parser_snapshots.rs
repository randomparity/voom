use insta::assert_yaml_snapshot;
use voom_dsl::parse_policy;

#[test]
fn snapshot_minimal_policy() {
    let input = r#"policy "minimal" {
        phase init {
            container mkv
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_config_block() {
    let input = r#"policy "with-config" {
        config {
            languages audio: [eng, und]
            languages subtitle: [eng, jpn]
            on_error: continue
            commentary_patterns: ["commentary", "director"]
        }
        phase init {
            container mkv
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_policy_extends_and_phase_extend() {
    let input = r#"policy "my-anime" extends "anime-base" {
        metadata {
            version: "1.2.0"
            author: "user@example.com"
            description: "Archive-quality anime preservation"
            requires_voom: ">=0.5.0"
            requires_tools: [ffmpeg, mkvmerge, mkvextract]
            test_fixtures: ["fixtures/anime/"]
        }

        phase audio {
            extend
            synthesize "AC3 5.1" {
                codec: ac3
                channels: 5.1
                source: prefer(channels >= 5.1 and lang == jpn)
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn format_preserves_policy_composition_syntax() {
    let input = r#"policy "child" extends "file://./base.voom" {
        metadata { version: "1.0.0" }
        phase audio { extend keep audio where lang == eng }
    }"#;
    let ast = parse_policy(input).unwrap();
    let formatted = voom_dsl::format_policy(&ast);

    assert!(formatted.contains("policy \"child\" extends \"file://./base.voom\""));
    assert!(formatted.contains("metadata {"));
    assert!(formatted.contains("extend"));
    let reparsed = parse_policy(&formatted).unwrap();
    assert_eq!(voom_dsl::format_policy(&reparsed), formatted);
}

#[test]
fn format_quotes_path_like_metadata_requires_tools() {
    let input = r#"policy "child" {
        metadata { requires_tools: ["tools/ffmpeg"] }
        phase audio { keep audio }
    }"#;
    let ast = parse_policy(input).unwrap();
    let formatted = voom_dsl::format_policy(&ast);

    assert!(formatted.contains(r#"requires_tools: ["tools/ffmpeg"]"#));
    let reparsed = parse_policy(&formatted).unwrap();
    assert_eq!(voom_dsl::format_policy(&reparsed), formatted);
}

#[test]
fn snapshot_keep_remove_operations() {
    let input = r#"policy "track-ops" {
        phase normalize {
            keep audio where lang in [eng, jpn, und]
            keep subtitles where lang in [eng] and not commentary
            remove attachments where not font
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_order_and_defaults() {
    let input = r#"policy "ordering" {
        phase normalize {
            order tracks [video, audio_main, subtitle_main, attachment]
            defaults {
                audio: first_per_language
                subtitle: none
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_actions_block() {
    let input = r#"policy "actions" {
        phase normalize {
            audio actions {
                clear_all_default: true
                clear_all_forced: true
                clear_all_titles: true
            }
            subtitle actions {
                clear_all_default: true
                clear_all_forced: true
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_transcode() {
    let input = r#"policy "transcode" {
        phase tc {
            skip when video.codec in [hevc, h265]
            transcode video to hevc {
                crf: 20
                preset: medium
            }
            transcode audio to aac {
                bitrate: 192k
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_synthesize() {
    let input = r#"policy "synth" {
        phase audio_compat {
            synthesize "Stereo AAC" {
                codec: aac
                channels: stereo
                source: prefer(codec in [truehd, dts_hd, flac] and channels >= 6)
                bitrate: "192k"
                skip_if_exists { codec in [aac] and channels == 2 and not commentary }
                title: "Stereo (AAC)"
                language: inherit
                position: after_source
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_when_block() {
    let input = r#"policy "when" {
        phase validate {
            when exists(audio where lang == jpn) and not exists(subtitle where lang == eng) {
                warn "Japanese audio but no English subtitles"
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_rules_block() {
    let input = r#"policy "rules" {
        phase validate {
            rules first {
                rule "multi-language" {
                    when audio_is_multi_language {
                        warn "Multiple audio languages"
                    }
                }
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_depends_on_and_run_if() {
    let input = r#"policy "deps" {
        phase first { container mkv }
        phase second {
            depends_on: [first]
            run_if first.modified
            container mkv
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_metadata_phase() {
    let input = r#"policy "metadata" {
        phase metadata {
            when plugin.radarr.original_language exists {
                set_language audio where default plugin.radarr.original_language
                set_tag "title" plugin.radarr.title
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_production_normalize() {
    let input = include_str!("fixtures/production-normalize.voom");
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_speech_language_filter_example() {
    let input = include_str!("../../../docs/examples/speech-language-filter.voom");
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_speech_transcription_check_example() {
    let input = include_str!("../../../docs/examples/speech-transcription-check.voom");
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn test_parse_error_has_location() {
    let result = parse_policy("not a valid policy");
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_str = format!("{err}");
    assert!(err_str.contains("parse error"));
}

#[test]
fn test_complex_filter_and_or() {
    let input = r#"policy "filters" {
        phase norm {
            keep audio where lang in [eng] and (codec in [aac] or codec in [flac])
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_when_else_block() {
    let input = r#"policy "when-else" {
        phase validate {
            when is_dubbed {
                warn "File is dubbed"
            } else {
                warn "File is not dubbed"
            }
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_container_metadata() {
    let input = include_str!("fixtures/container-metadata.voom");
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

// === Escape sequence test ===

#[test]
fn test_escape_sequences_in_strings() {
    let input = r#"policy "test \"escapes\"" {
        phase clean {
            set_tag "path" "C:\\Media\\Movies"
            set_tag "note" "contains \"quotes\""
        }
    }"#;
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, r#"test "escapes""#);

    // Verify round-trip through formatter preserves escapes
    let formatted = voom_dsl::format_policy(&ast);
    let ast2 = parse_policy(&formatted).unwrap();
    assert_eq!(ast2.name, ast.name);
    assert_eq!(ast2.phases.len(), ast.phases.len());
}

// === Example policy parsing tests ===
// Verify all sample policies in docs/examples/ are syntactically valid.

#[test]
fn example_minimal_parses() {
    let input = include_str!("../../../docs/examples/minimal.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "minimal");
    assert_eq!(ast.phases.len(), 1);
}

#[test]
fn example_movie_library_parses() {
    let input = include_str!("../../../docs/examples/movie-library.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "movie-library");
    assert_eq!(ast.phases.len(), 4);
    assert!(ast.config.is_some());
}

#[test]
fn example_anime_collection_parses() {
    let input = include_str!("../../../docs/examples/anime-collection.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "anime-collection");
    assert_eq!(ast.phases.len(), 5);
}

#[test]
fn example_transcode_hevc_parses() {
    let input = include_str!("../../../docs/examples/transcode-hevc.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "transcode-hevc");
    assert_eq!(ast.phases.len(), 5);
}

#[test]
fn example_containerize_then_transcode_parses_and_validates() {
    let input = include_str!("../../../docs/examples/containerize-then-transcode.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "containerize-then-transcode");
    assert_eq!(ast.phases.len(), 2);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_continue_on_error_transcode_parses_and_validates() {
    let input = include_str!("../../../docs/examples/continue-on-error-transcode.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "continue-on-error-transcode");
    assert_eq!(ast.phases.len(), 1);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_hw_nvenc_hevc_parses_and_validates() {
    let input = include_str!("../../../docs/examples/hw-nvenc-hevc.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "hw-nvenc-hevc");
    assert_eq!(ast.phases.len(), 1);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_transcode_video_drop_attachments_parses_and_validates() {
    let input = include_str!("../../../docs/examples/transcode-video-drop-attachments.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "transcode-video-drop-attachments");
    assert_eq!(ast.phases.len(), 1);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_metadata_stable_transcode_parses_and_validates() {
    let input = include_str!("../../../docs/examples/metadata-stable-transcode.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "metadata-stable-transcode");
    assert_eq!(ast.phases.len(), 1);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_hdr_archival_parses_and_validates() {
    let input = include_str!("../../../docs/examples/hdr-archival.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "hdr-archival");
    assert_eq!(ast.phases.len(), 2);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_hdr_sdr_mobile_parses_and_validates() {
    let input = include_str!("../../../docs/examples/hdr-sdr-mobile.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "hdr-sdr-mobile");
    assert_eq!(ast.phases.len(), 1);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_vmaf_guided_parses_and_validates() {
    let input = include_str!("../../../docs/examples/vmaf-guided.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "vmaf-guided");
    assert_eq!(ast.phases.len(), 1);
    let result = voom_dsl::validate(&ast);
    assert!(
        result.is_ok(),
        "validation errors: {:?}",
        result.unwrap_err().errors
    );
}

#[test]
fn example_remote_backup_transcode_parses_and_validates() {
    let input = include_str!("../../../docs/examples/remote-backup-transcode.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "remote-backup-transcode");
    assert_eq!(ast.phases.len(), 2);
    voom_dsl::validate(&ast).unwrap();
}

#[test]
fn example_metadata_enrichment_parses() {
    let input = include_str!("../../../docs/examples/metadata-enrichment.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "metadata-enrichment");
    assert_eq!(ast.phases.len(), 5);
}

#[test]
fn example_strict_archive_parses() {
    let input = include_str!("../../../docs/examples/strict-archive.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "strict-archive");
    assert_eq!(ast.phases.len(), 5);
}

#[test]
fn example_attachment_management_parses() {
    let input = include_str!("../../../docs/examples/attachment-management.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "attachment-management");
    assert_eq!(ast.phases.len(), 3);
}

#[test]
fn example_full_pipeline_parses_and_validates() {
    let input = include_str!("../../../docs/examples/full-pipeline.voom");
    let ast = parse_policy(input).unwrap();
    assert_eq!(ast.name, "full-pipeline");
    assert_eq!(ast.phases.len(), 12);
    assert!(ast.config.is_some());
    // Validate semantics (codecs, languages, phase refs, etc.)
    let result = voom_dsl::validate(&ast);
    assert!(
        result.is_ok(),
        "validation errors: {:?}",
        result.unwrap_err().errors
    );
}
