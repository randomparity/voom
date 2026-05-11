/// Return the stable names of policies bundled into the DSL crate.
pub fn bundled_policy_names() -> [&'static str; 5] {
    [
        "archival",
        "space-saver",
        "mobile-friendly",
        "anime-base",
        "passthrough-audit",
    ]
}

/// Return the source text for a bundled policy by name.
pub fn bundled_policy(name: &str) -> Option<&'static str> {
    match name {
        "archival" => Some(include_str!("bundled/archival.voom")),
        "space-saver" => Some(include_str!("bundled/space-saver.voom")),
        "mobile-friendly" => Some(include_str!("bundled/mobile-friendly.voom")),
        "anime-base" => Some(include_str!("bundled/anime-base.voom")),
        "passthrough-audit" => Some(include_str!("bundled/passthrough-audit.voom")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{bundled_policy, bundled_policy_names, parse_policy, validate};

    #[test]
    fn exposes_expected_bundled_policy_names() {
        assert_eq!(
            bundled_policy_names(),
            [
                "archival",
                "space-saver",
                "mobile-friendly",
                "anime-base",
                "passthrough-audit"
            ]
        );
    }

    #[test]
    fn bundled_policies_parse() {
        for name in bundled_policy_names() {
            let source = bundled_policy(name).expect("bundled policy exists");
            let ast = parse_policy(source).expect(name);
            assert_eq!(ast.name, name);
            validate(&ast).expect(name);
            assert!(ast.extends.is_none());
        }
    }

    #[test]
    fn unknown_bundled_policy_returns_none() {
        assert!(bundled_policy("registry://anime-base").is_none());
    }
}
