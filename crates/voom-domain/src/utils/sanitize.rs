use crate::errors::VoomError;

/// Validates that a metadata value does not contain dangerous control characters.
/// Allows tab (\t) and newline (\n, \r) but rejects other control characters (\x00-\x1F).
///
/// # Errors
///
/// Returns [`VoomError::Validation`] if the value contains control characters.
pub fn validate_metadata_value(value: &str) -> Result<&str, VoomError> {
    if let Some(pos) = value
        .bytes()
        .position(|b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r')
    {
        return Err(VoomError::Validation(format!(
            "metadata value contains control character at byte {}: 0x{:02x}",
            pos,
            value.as_bytes()[pos]
        )));
    }
    Ok(value)
}

/// Validates a metadata key (tag name) for safe use in command-line arguments.
///
/// Rejects control characters (like [`validate_metadata_value`]) and additionally
/// rejects `=` which would confuse `FFmpeg`'s `key=value` metadata argument parsing.
///
/// # Errors
///
/// Returns [`VoomError::Validation`] if the key is empty, contains control characters,
/// or contains `=`.
pub fn validate_metadata_key(key: &str) -> Result<&str, VoomError> {
    if key.is_empty() {
        return Err(VoomError::Validation(
            "metadata key cannot be empty".to_string(),
        ));
    }
    validate_metadata_value(key)?;
    if key.contains('=') {
        return Err(VoomError::Validation(format!(
            "metadata key contains '=' which would corrupt key=value parsing: {key:?}"
        )));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_strings_pass() {
        assert!(validate_metadata_value("Hello World").is_ok());
        assert!(validate_metadata_value("English Stereo 5.1").is_ok());
        assert!(validate_metadata_value("").is_ok());
        assert!(validate_metadata_value("Unicode: \u{00e9}\u{00f1}\u{00fc}").is_ok());
    }

    #[test]
    fn test_tabs_and_newlines_pass() {
        assert!(validate_metadata_value("line1\nline2").is_ok());
        assert!(validate_metadata_value("col1\tcol2").is_ok());
        assert!(validate_metadata_value("line1\r\nline2").is_ok());
    }

    #[test]
    fn test_null_byte_fails() {
        let result = validate_metadata_value("hello\x00world");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("0x00"), "error should mention 0x00: {err}");
    }

    #[test]
    fn test_control_char_0x01_fails() {
        let result = validate_metadata_value("bad\x01value");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("0x01"), "error should mention 0x01: {err}");
    }

    #[test]
    fn test_control_char_0x1f_fails() {
        let result = validate_metadata_value("bad\x1fvalue");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("0x1f"), "error should mention 0x1f: {err}");
    }

    #[test]
    fn test_reports_correct_byte_position() {
        let result = validate_metadata_value("abc\x05def");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("byte 3"), "error should report byte 3: {err}");
    }

    // ── validate_metadata_key ─────────────────────────────────────

    #[test]
    fn test_key_normal_passes() {
        assert!(validate_metadata_key("title").is_ok());
        assert!(validate_metadata_key("ENCODER").is_ok());
    }

    #[test]
    fn test_key_empty_fails() {
        assert!(validate_metadata_key("").is_err());
    }

    #[test]
    fn test_key_with_equals_fails() {
        let result = validate_metadata_key("key=value");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains('='), "error should mention '=': {err}");
    }

    #[test]
    fn test_key_with_control_char_fails() {
        assert!(validate_metadata_key("bad\x00key").is_err());
    }
}
