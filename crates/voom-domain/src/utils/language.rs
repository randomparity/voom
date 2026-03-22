use std::collections::HashMap;
use std::sync::LazyLock;

/// ISO 639-2/B language codes used in media containers.
/// Maps both 2-letter (639-1) and 3-letter (639-2/B) codes to the canonical 3-letter code.
static LANGUAGE_CODES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    let entries = [
        ("en", "eng"),
        ("eng", "eng"),
        ("fr", "fre"),
        ("fra", "fre"),
        ("fre", "fre"),
        ("de", "ger"),
        ("deu", "ger"),
        ("ger", "ger"),
        ("es", "spa"),
        ("spa", "spa"),
        ("it", "ita"),
        ("ita", "ita"),
        ("pt", "por"),
        ("por", "por"),
        ("ru", "rus"),
        ("rus", "rus"),
        ("ja", "jpn"),
        ("jpn", "jpn"),
        ("zh", "chi"),
        ("zho", "chi"),
        ("chi", "chi"),
        ("ko", "kor"),
        ("kor", "kor"),
        ("ar", "ara"),
        ("ara", "ara"),
        ("hi", "hin"),
        ("hin", "hin"),
        ("nl", "dut"),
        ("nld", "dut"),
        ("dut", "dut"),
        ("sv", "swe"),
        ("swe", "swe"),
        ("no", "nor"),
        ("nor", "nor"),
        ("da", "dan"),
        ("dan", "dan"),
        ("fi", "fin"),
        ("fin", "fin"),
        ("pl", "pol"),
        ("pol", "pol"),
        ("cs", "cze"),
        ("ces", "cze"),
        ("cze", "cze"),
        ("hu", "hun"),
        ("hun", "hun"),
        ("ro", "rum"),
        ("ron", "rum"),
        ("rum", "rum"),
        ("el", "gre"),
        ("ell", "gre"),
        ("gre", "gre"),
        ("tr", "tur"),
        ("tur", "tur"),
        ("he", "heb"),
        ("heb", "heb"),
        ("th", "tha"),
        ("tha", "tha"),
        ("vi", "vie"),
        ("vie", "vie"),
        ("uk", "ukr"),
        ("ukr", "ukr"),
        ("und", "und"),
    ];
    for (key, val) in entries {
        m.insert(key, val);
    }
    m
});

/// Language display names for the canonical 3-letter codes.
static LANGUAGE_NAMES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    let entries = [
        ("eng", "English"),
        ("fre", "French"),
        ("ger", "German"),
        ("spa", "Spanish"),
        ("ita", "Italian"),
        ("por", "Portuguese"),
        ("rus", "Russian"),
        ("jpn", "Japanese"),
        ("chi", "Chinese"),
        ("kor", "Korean"),
        ("ara", "Arabic"),
        ("hin", "Hindi"),
        ("dut", "Dutch"),
        ("swe", "Swedish"),
        ("nor", "Norwegian"),
        ("dan", "Danish"),
        ("fin", "Finnish"),
        ("pol", "Polish"),
        ("cze", "Czech"),
        ("hun", "Hungarian"),
        ("rum", "Romanian"),
        ("gre", "Greek"),
        ("tur", "Turkish"),
        ("heb", "Hebrew"),
        ("tha", "Thai"),
        ("vie", "Vietnamese"),
        ("ukr", "Ukrainian"),
        ("und", "Undetermined"),
    ];
    for (key, val) in entries {
        m.insert(key, val);
    }
    m
});

/// Validate and normalize a language code to its canonical ISO 639-2/B form.
/// Returns `None` if the code is not recognized.
pub fn normalize_language(code: &str) -> Option<&'static str> {
    LANGUAGE_CODES
        .get(code.to_ascii_lowercase().as_str())
        .copied()
}

/// Check if a language code is valid (either ISO 639-1 or 639-2/B).
pub fn is_valid_language(code: &str) -> bool {
    LANGUAGE_CODES.contains_key(code.to_ascii_lowercase().as_str())
}

/// Get the display name for a canonical language code.
pub fn language_name(code: &str) -> Option<&'static str> {
    let canonical = normalize_language(code)?;
    LANGUAGE_NAMES.get(canonical).copied()
}

/// All known canonical 3-letter language codes (cached, sorted).
static ALL_LANGUAGE_CODES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut codes: Vec<&str> = LANGUAGE_NAMES.keys().copied().collect();
    codes.sort_unstable();
    codes
});

/// Returns all known canonical 3-letter language codes.
#[must_use]
pub fn all_language_codes() -> &'static [&'static str] {
    &ALL_LANGUAGE_CODES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_2_letter() {
        assert_eq!(normalize_language("en"), Some("eng"));
        assert_eq!(normalize_language("fr"), Some("fre"));
        assert_eq!(normalize_language("de"), Some("ger"));
        assert_eq!(normalize_language("ja"), Some("jpn"));
    }

    #[test]
    fn test_normalize_3_letter() {
        assert_eq!(normalize_language("eng"), Some("eng"));
        assert_eq!(normalize_language("fre"), Some("fre"));
        assert_eq!(normalize_language("fra"), Some("fre"));
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(normalize_language("ENG"), Some("eng"));
        assert_eq!(normalize_language("Fr"), Some("fre"));
    }

    #[test]
    fn test_invalid_language() {
        assert_eq!(normalize_language("xyz"), None);
        assert!(!is_valid_language("zzz"));
    }

    #[test]
    fn test_language_name() {
        assert_eq!(language_name("eng"), Some("English"));
        assert_eq!(language_name("en"), Some("English"));
        assert_eq!(language_name("jpn"), Some("Japanese"));
    }

    #[test]
    fn test_und() {
        assert_eq!(normalize_language("und"), Some("und"));
        assert_eq!(language_name("und"), Some("Undetermined"));
    }
}
