//! Internal helpers shared across verifier modes.

/// UTF-8-safe truncation. Returns `s` unchanged when its byte-length is
/// `<= max`. Otherwise cuts at the largest char boundary `<= max` and
/// appends a `...[truncated]` marker. Never panics on multi-byte
/// boundaries.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= max)
        .last()
        .unwrap_or(0);
    let mut t = s[..cut].to_string();
    t.push_str("...[truncated]");
    t
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncates_long_ascii() {
        let s = "a".repeat(5000);
        let t = truncate(&s, 4096);
        assert!(t.len() <= 4096 + "...[truncated]".len());
        assert!(t.ends_with("[truncated]"));
    }

    #[test]
    fn truncate_handles_multibyte_safely() {
        // Each "ä" is 2 bytes in UTF-8; cutting at an odd offset must
        // not panic.
        let s = "ä".repeat(5000);
        let t = truncate(&s, 100);
        let marker_len = "...[truncated]".len();
        assert!(t.len() <= 100 + marker_len);
        assert!(t.ends_with("[truncated]"));
    }

    #[test]
    fn shorter_than_max_returns_input() {
        let s = "hello";
        assert_eq!(truncate(s, 100), "hello");
    }
}
