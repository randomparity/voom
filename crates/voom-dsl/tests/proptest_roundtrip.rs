//! Property-based test: format/parse roundtrip preserves AST semantics.

use proptest::prelude::*;
use voom_dsl::testing::strategies::policy_ast_strategy;
use voom_dsl::{format_policy, parse_policy};

/// Strip `span` fields so two ASTs that differ only in source positions compare equal.
fn normalize(ast: &voom_dsl::PolicyAst) -> serde_json::Value {
    let mut json = serde_json::to_value(ast).expect("AST is serializable");
    strip_spans(&mut json);
    json
}

fn strip_spans(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("span");
            for v in map.values_mut() {
                strip_spans(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                strip_spans(v);
            }
        }
        _ => {}
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn format_parse_roundtrip(ast in policy_ast_strategy()) {
        let source = format_policy(&ast);
        let reparsed = parse_policy(&source).unwrap_or_else(|e| {
            panic!("failed to reparse formatted output:\n---\n{source}\n---\nerror: {e:?}")
        });
        prop_assert_eq!(normalize(&ast), normalize(&reparsed));
    }
}
