//! Verify that every container extension known to voom-domain is accepted
//! by both the DSL validator and the DSL compiler.

use voom_domain::media::Container;
use voom_dsl::{compile_policy, parse_policy, validate};

#[test]
fn every_known_container_is_accepted_by_compiler_and_validator() {
    for ext in Container::known_extensions() {
        let source = format!(
            r#"
policy "test-{ext}" {{
  phase init {{
    container {ext}
  }}
}}
"#
        );
        let ast = parse_policy(&source).unwrap_or_else(|e| panic!("parse failed for {ext}: {e}"));
        validate(&ast).unwrap_or_else(|e| panic!("validate failed for {ext}: {e:?}"));
        compile_policy(&source).unwrap_or_else(|e| panic!("compile failed for {ext}: {e}"));
    }
}

#[test]
fn unknown_container_rejected_with_helpful_message() {
    let source = r#"
policy "bad" {
  phase init {
    container xyz
  }
}
"#;
    let ast = parse_policy(source).unwrap();
    let err = validate(&ast).unwrap_err();
    let msg = format!("{}", err.errors[0]);
    assert!(msg.contains("unknown container 'xyz'"), "got: {msg}");
    assert!(msg.contains("m2ts"), "message should list m2ts: {msg}");
}

#[test]
fn codec_field_path_is_validated() {
    // FilterNode::CodecField (codec == plugin.X.Y) routes through the
    // shared validate_field_path helper. Lock in the current behaviour:
    // a known field root ("plugin") is accepted without error, while an
    // unknown root produces a validation error.
    let ok_source = r#"policy "codec-field-ok" {
        phase init {
            keep audio where codec == plugin.detector.codec
            container mkv
        }
    }"#;
    let ast = parse_policy(ok_source).unwrap();
    validate(&ast).expect("plugin.detector.codec should be accepted");

    let bad_source = r#"policy "codec-field-bad" {
        phase init {
            keep audio where codec == bogus_root.codec
            container mkv
        }
    }"#;
    let ast = parse_policy(bad_source).unwrap();
    let err = validate(&ast).expect_err("bogus_root should be rejected");
    assert!(
        err.errors
            .iter()
            .any(|e| format!("{e}").contains("unknown field root")),
        "expected unknown field root error, got: {:?}",
        err.errors
    );
}
