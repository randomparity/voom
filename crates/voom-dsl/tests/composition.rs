use std::fs;

use tempfile::tempdir;
use voom_dsl::{compile_policy, compile_policy_file, compile_policy_with_bundled};

#[test]
fn bundled_extends_inherits_missing_phases() {
    let policy = compile_policy_with_bundled(
        r#"policy "child" extends "anime-base" {
            phase subtitles {
                keep subtitles where lang == eng
            }
        }"#,
    )
    .unwrap();

    assert!(
        policy
            .phases
            .iter()
            .any(|phase| phase.name == "containerize")
    );
    assert!(policy.phases.iter().any(|phase| phase.name == "audio"));
    let subtitles = policy
        .phases
        .iter()
        .find(|phase| phase.name == "subtitles")
        .unwrap();
    assert_eq!(subtitles.operations.len(), 1);
}

#[test]
fn phase_extend_appends_operations_and_inherits_controls() {
    let policy = compile_policy_with_bundled(
        r#"policy "child" extends "anime-base" {
            phase audio {
                extend
                synthesize "AAC Stereo" {
                    codec: aac
                    channels: stereo
                    source: prefer(channels >= 6)
                }
            }
        }"#,
    )
    .unwrap();

    let audio = policy
        .phases
        .iter()
        .find(|phase| phase.name == "audio")
        .unwrap();
    assert_eq!(audio.depends_on, ["containerize"]);
    assert!(audio.operations.len() > 1);
}

#[test]
fn phase_extend_can_clear_inherited_dependencies() {
    let policy = compile_policy_with_bundled(
        r#"policy "child" extends "anime-base" {
            phase audio {
                extend
                depends_on: []
                keep audio where lang == jpn
            }
        }"#,
    )
    .unwrap();

    let audio = policy
        .phases
        .iter()
        .find(|phase| phase.name == "audio")
        .unwrap();
    assert!(audio.depends_on.is_empty());
}

#[test]
fn file_extends_resolves_relative_to_child_file() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("shared")).unwrap();
    fs::write(
        dir.path().join("shared/base.voom"),
        r#"policy "base" { phase base { container mkv } }"#,
    )
    .unwrap();
    let child = dir.path().join("child.voom");
    fs::write(
        &child,
        r#"policy "child" extends "file://./shared/base.voom" {
            phase child { depends_on: [base] keep audio }
        }"#,
    )
    .unwrap();

    let policy = compile_policy_file(&child).unwrap();

    assert_eq!(policy.phase_order, ["base", "child"]);
}

#[test]
fn cyclic_extends_reports_chain() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.voom");
    let b = dir.path().join("b.voom");
    fs::write(
        &a,
        r#"policy "a" extends "file://./b.voom" { phase a { container mkv } }"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"policy "b" extends "file://./a.voom" { phase b { container mkv } }"#,
    )
    .unwrap();

    let err = compile_policy_file(&a).unwrap_err().to_string();

    assert!(err.contains("cyclic policy extends"));
    assert!(err.contains("a.voom"));
    assert!(err.contains("b.voom"));
}

#[test]
fn extending_unknown_phase_is_rejected() {
    let err = compile_policy_with_bundled(
        r#"policy "child" extends "anime-base" {
            phase missing {
                extend
                keep audio
            }
        }"#,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("phase \"missing\" uses extend but no parent phase exists"));
}

#[test]
fn inherited_root_policy_with_unresolved_phase_extend_is_rejected() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.voom");
    let child = dir.path().join("child.voom");
    fs::write(
        &base,
        r#"policy "base" {
            phase base {
                extend
                keep audio
            }
        }"#,
    )
    .unwrap();
    fs::write(
        &child,
        r#"policy "child" extends "file://./base.voom" {
            phase child {
                keep subtitles
            }
        }"#,
    )
    .unwrap();

    let err = compile_policy_file(&child).unwrap_err().to_string();

    assert!(err.contains("phase \"base\" uses extend but policy composition was not resolved"));
}

#[test]
fn compile_policy_rejects_unresolved_bundled_extends() {
    let err = compile_policy(
        r#"policy "child" extends "anime-base" {
            phase subtitles {
                keep subtitles
            }
        }"#,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("policy extends requires composition resolution"));
    assert!(err.contains("compile_policy_with_bundled(source)"));
}

#[test]
fn unsupported_extends_scheme_is_rejected() {
    let err = compile_policy_with_bundled(
        r#"policy "child" extends "registry://x" {
            phase subtitles {
                keep subtitles
            }
        }"#,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("unsupported policy extends URI \"registry://x\""));
}

#[test]
fn empty_file_extends_uri_is_rejected() {
    let err = compile_policy_with_bundled(
        r#"policy "child" extends "file://" {
            phase subtitles {
                keep subtitles
            }
        }"#,
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("file:// policy extends URI must include a path"));
}

#[test]
fn composed_source_hash_reflects_resolved_parent_behavior() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(
        first.join("base.voom"),
        r#"policy "base" { phase base { container mkv } }"#,
    )
    .unwrap();
    fs::write(
        second.join("base.voom"),
        r#"policy "base" { phase base { container mp4 } }"#,
    )
    .unwrap();
    let child_source = r#"policy "child" extends "file://./base.voom" {
        phase child { keep audio }
    }"#;
    let first_child = first.join("child.voom");
    let second_child = second.join("child.voom");
    fs::write(&first_child, child_source).unwrap();
    fs::write(&second_child, child_source).unwrap();

    let first_policy = compile_policy_file(&first_child).unwrap();
    let second_policy = compile_policy_file(&second_child).unwrap();

    assert_ne!(first_policy.source_hash, second_policy.source_hash);
}
