use std::collections::BTreeMap;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "mov", "avi", "m4v", "webm", "ts", "m2ts", "mpg", "mpeg",
];

#[derive(Debug, Default)]
pub struct Redactor {
    replacements: BTreeMap<String, String>,
    kinds: BTreeMap<String, RedactionKind>,
    video_count: usize,
    api_key_count: usize,
    token_count: usize,
    secret_count: usize,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct RedactionReport {
    pub public_mappings: Vec<PublicRedactionMapping>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrivateRedactionMapping {
    pub original: String,
    pub replacement: String,
    pub kind: RedactionKind,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicRedactionMapping {
    pub replacement: String,
    pub kind: RedactionKind,
}

#[derive(Debug)]
pub struct RedactionSnapshot {
    pub public: RedactionReport,
    pub private: Vec<PrivateRedactionMapping>,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionKind {
    FileName,
    Secret,
    PathComponent,
    TrackComment,
}

impl Redactor {
    pub fn redact_text(&mut self, input: &str) -> String {
        self.register_secret_assignments(input);
        self.register_video_filenames(input);
        self.apply_replacements(input)
    }

    pub fn redact_json(&mut self, value: serde_json::Value) -> serde_json::Value {
        self.redact_json_with_context(value, &RedactionContext::default())
    }

    fn redact_json_with_context(
        &mut self,
        value: serde_json::Value,
        context: &RedactionContext,
    ) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => self.redact_object_with_context(map, context),
            serde_json::Value::Array(values) => serde_json::Value::Array(
                values
                    .into_iter()
                    .map(|value| self.redact_json_with_context(value, context))
                    .collect(),
            ),
            serde_json::Value::String(s) => serde_json::Value::String(self.redact_text(&s)),
            other => other,
        }
    }

    fn redact_object_with_context(
        &mut self,
        map: serde_json::Map<String, serde_json::Value>,
        context: &RedactionContext,
    ) -> serde_json::Value {
        let context = context.with_file(self.file_context_from_map(&map));
        let track_comment = context
            .file
            .as_ref()
            .and_then(|file| track_comment_replacement(file, &map));
        let context = context.with_track_comment(track_comment);

        serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let value = self.redact_object_value(key.as_str(), value, &context);
                    (key, value)
                })
                .collect(),
        )
    }

    fn redact_object_value(
        &mut self,
        key: &str,
        value: serde_json::Value,
        context: &RedactionContext,
    ) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => {
                if is_secret_key(key) {
                    let replacement = self.secret_replacement_for_key(key);
                    self.register_replacement(s, replacement.clone(), RedactionKind::Secret);
                    return serde_json::Value::String(replacement);
                }
                if key.eq_ignore_ascii_case("comment") {
                    if let Some(replacement) = &context.track_comment {
                        self.register_replacement(
                            s,
                            replacement.clone(),
                            RedactionKind::TrackComment,
                        );
                        return serde_json::Value::String(replacement.clone());
                    }
                }
                serde_json::Value::String(self.redact_text(&s))
            }
            serde_json::Value::Object(map) => self.redact_object_with_context(map, context),
            serde_json::Value::Array(values) => serde_json::Value::Array(
                values
                    .into_iter()
                    .map(|value| self.redact_json_with_context(value, context))
                    .collect(),
            ),
            other => other,
        }
    }

    fn file_context_from_map(
        &mut self,
        map: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<FileContext> {
        let source = map
            .get("filename")
            .and_then(serde_json::Value::as_str)
            .or_else(|| map.get("path").and_then(serde_json::Value::as_str))?;
        let placeholder = self.file_placeholder_for_source(source)?;
        Some(FileContext { placeholder })
    }

    fn file_placeholder_for_source(&mut self, source: &str) -> Option<String> {
        let file_name = source.rsplit(['/', '\\']).next().unwrap_or(source);
        if !is_video_filename(file_name) {
            return None;
        }

        if let Some(replacement) = self.replacements.get(file_name) {
            return Some(replacement.clone());
        }

        let ext = extension(file_name)?;
        let replacement = format!("video{:03}.{ext}", self.video_count);
        self.video_count += 1;
        self.register_replacement(
            file_name.to_string(),
            replacement.clone(),
            RedactionKind::FileName,
        );
        Some(replacement)
    }

    pub fn private_mappings(&self) -> Vec<PrivateRedactionMapping> {
        self.replacements
            .iter()
            .map(|(original, replacement)| PrivateRedactionMapping {
                original: original.clone(),
                replacement: replacement.clone(),
                kind: self
                    .kinds
                    .get(original)
                    .copied()
                    .unwrap_or(RedactionKind::PathComponent),
            })
            .collect()
    }

    pub fn snapshot(&self) -> RedactionSnapshot {
        let private = self.private_mappings();
        let public = RedactionReport {
            public_mappings: private
                .iter()
                .map(|mapping| PublicRedactionMapping {
                    replacement: mapping.replacement.clone(),
                    kind: mapping.kind,
                })
                .collect(),
        };
        RedactionSnapshot { public, private }
    }

    fn register_secret_assignments(&mut self, input: &str) {
        for token in input.split_whitespace() {
            let Some((key, value)) = split_secret_assignment(token) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            let replacement = self.secret_replacement_for_key(key);
            self.register_replacement(value.to_string(), replacement, RedactionKind::Secret);
        }
    }

    fn register_video_filenames(&mut self, input: &str) {
        for candidate in filename_candidates(input) {
            if !is_video_filename(candidate) {
                continue;
            }
            if self.replacements.contains_key(candidate) {
                continue;
            }
            let ext = extension(candidate).expect("video filename has extension");
            let replacement = format!("video{:03}.{ext}", self.video_count);
            self.video_count += 1;
            self.register_replacement(candidate.to_string(), replacement, RedactionKind::FileName);
        }
    }

    fn register_replacement(&mut self, original: String, replacement: String, kind: RedactionKind) {
        self.replacements
            .entry(original.clone())
            .or_insert_with(|| replacement);
        self.kinds.entry(original).or_insert(kind);
    }

    fn apply_replacements(&self, input: &str) -> String {
        let mut output = input.to_string();
        let mut mappings: Vec<_> = self.replacements.iter().collect();
        mappings.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
        for (original, replacement) in mappings {
            output = output.replace(original, replacement);
        }
        output
    }

    fn secret_replacement_for_key(&mut self, key: &str) -> String {
        let category = secret_category(key);
        match category {
            SecretCategory::ApiKey => {
                self.api_key_count += 1;
                format!("<api-key-{:03}>", self.api_key_count)
            }
            SecretCategory::Token => {
                self.token_count += 1;
                format!("<token-{:03}>", self.token_count)
            }
            SecretCategory::Secret => {
                self.secret_count += 1;
                format!("<secret-{:03}>", self.secret_count)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct FileContext {
    placeholder: String,
}

#[derive(Debug, Clone, Default)]
struct RedactionContext {
    file: Option<FileContext>,
    track_comment: Option<String>,
}

impl RedactionContext {
    fn with_file(&self, file: Option<FileContext>) -> Self {
        Self {
            file: file.or_else(|| self.file.clone()),
            track_comment: self.track_comment.clone(),
        }
    }

    fn with_track_comment(&self, track_comment: Option<String>) -> Self {
        Self {
            file: self.file.clone(),
            track_comment: track_comment.or_else(|| self.track_comment.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SecretCategory {
    ApiKey,
    Token,
    Secret,
}

fn track_comment_replacement(
    file_context: &FileContext,
    track: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let track_kind = normalized_track_kind(
        track
            .get("track_type")
            .or_else(|| track.get("type"))
            .and_then(serde_json::Value::as_str)?,
    )?;
    let index = track.get("index").and_then(serde_json::Value::as_u64)?;
    Some(format!(
        "comment for {} {} track {}",
        file_context.placeholder, track_kind, index
    ))
}

fn normalized_track_kind(track_type: &str) -> Option<&'static str> {
    let normalized = track_type.to_ascii_lowercase();
    if normalized == "video" {
        Some("video")
    } else if normalized.starts_with("audio") {
        Some("audio")
    } else if normalized.starts_with("subtitle") {
        Some("subtitle")
    } else if normalized == "attachment" {
        Some("attachment")
    } else {
        None
    }
}

fn split_secret_assignment(token: &str) -> Option<(&str, &str)> {
    let (key, value) = token.split_once('=')?;
    if !is_secret_key(key) {
        return None;
    }
    Some((key, value.trim_matches('"').trim_matches('\'')))
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    normalized.contains("token")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("credential")
        || normalized.contains("bearer")
}

fn secret_category(key: &str) -> SecretCategory {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    if normalized.contains("api_key") || normalized.contains("apikey") {
        SecretCategory::ApiKey
    } else if normalized.contains("token") || normalized.contains("bearer") {
        SecretCategory::Token
    } else {
        SecretCategory::Secret
    }
}

fn filename_candidates(input: &str) -> Vec<&str> {
    let mut candidates = Vec::new();

    for ext in VIDEO_EXTENSIONS {
        let needle = format!(".{ext}");
        let mut search_start = 0usize;
        while let Some(offset) = input[search_start..].find(&needle) {
            let ext_start = search_start + offset;
            let ext_end = ext_start + needle.len();
            let start = candidate_start(input, ext_start);
            let candidate = input[start..ext_end].trim_matches(|ch: char| {
                ch.is_whitespace() || matches!(ch, '"' | '\'' | '[' | ']' | '{' | '}')
            });
            if !candidate.is_empty() {
                candidates.push(candidate);
            }
            search_start = ext_end;
        }
    }

    candidates
}

fn candidate_start(input: &str, ext_start: usize) -> usize {
    let prefix = &input[..ext_start];
    for (idx, ch) in prefix.char_indices().rev() {
        if matches!(ch, '/' | '\\' | '"' | '\'' | '\t' | '\n' | '\r') {
            return idx + ch.len_utf8();
        }
    }

    let mut starts: Vec<usize> = prefix
        .char_indices()
        .filter_map(|(idx, ch)| ch.is_whitespace().then_some(idx + ch.len_utf8()))
        .collect();
    starts.insert(0, 0);

    for start in starts {
        let candidate = prefix[start..].trim_start();
        let Some((first_word, _)) = candidate.split_once(char::is_whitespace) else {
            continue;
        };
        if first_word
            .chars()
            .next()
            .is_some_and(|ch| ch.is_uppercase() || ch.is_ascii_digit())
        {
            return start + (prefix[start..].len() - candidate.len());
        }
    }

    0
}

fn is_video_filename(candidate: &str) -> bool {
    extension(candidate).is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext))
}

fn extension(candidate: &str) -> Option<&str> {
    let (_, ext) = candidate.rsplit_once('.')?;
    let ext = ext.trim_end_matches([')', ']', '}', ',', ';', ':']);
    if ext.is_empty() {
        return None;
    }
    Some(ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_same_video_filename_consistently() {
        let mut redactor = Redactor::default();
        let first = redactor.redact_text("/media/The Movie (2026).mkv failed");
        let second = redactor.redact_text("retry The Movie (2026).mkv now");

        assert_eq!(first, "/media/video000.mkv failed");
        assert_eq!(second, "retry video000.mkv now");
    }

    #[test]
    fn redacts_same_secret_value_consistently() {
        let mut redactor = Redactor::default();
        let first = redactor.redact_text("api_key=sk-123456");
        let second = redactor.redact_text("token sk-123456 failed");

        assert_eq!(first, "api_key=<api-key-001>");
        assert_eq!(second, "token <api-key-001> failed");
    }

    #[test]
    fn redacts_json_recursively() {
        let mut redactor = Redactor::default();
        let value = serde_json::json!({
            "path": "/media/The Movie (2026).mkv",
            "env": {"OPENAI_API_KEY": "sk-123456"}
        });

        let redacted = redactor.redact_json(value);

        assert_eq!(redacted["path"], "/media/video000.mkv");
        assert_eq!(redacted["env"]["OPENAI_API_KEY"], "<api-key-001>");
    }

    #[test]
    fn redacts_video_track_comment_with_file_context() {
        let mut redactor = Redactor::default();
        let value = serde_json::json!({
            "path": "/media/The Movie (2026).mkv",
            "tracks": [{
                "index": 1,
                "track_type": "video",
                "comment": "Shot on a private camera"
            }]
        });

        let redacted = redactor.redact_json(value);

        assert_eq!(redacted["path"], "/media/video000.mkv");
        assert_eq!(
            redacted["tracks"][0]["comment"],
            "comment for video000.mkv video track 1"
        );
        assert!(redactor.private_mappings().iter().any(|m| {
            m.original == "Shot on a private camera"
                && m.replacement == "comment for video000.mkv video track 1"
        }));
    }

    #[test]
    fn redacts_audio_and_subtitle_track_tag_comments() {
        let mut redactor = Redactor::default();
        let value = serde_json::json!({
            "filename": "The Movie (2026).mkv",
            "tracks": [
                {
                    "index": 2,
                    "track_type": "audio_commentary",
                    "tags": {"COMMENT": "Private audio note"}
                },
                {
                    "index": 3,
                    "track_type": "subtitle_forced",
                    "tags": {"comment": "Private subtitle note"}
                }
            ]
        });

        let redacted = redactor.redact_json(value);

        assert_eq!(redacted["filename"], "video000.mkv");
        assert_eq!(
            redacted["tracks"][0]["tags"]["COMMENT"],
            "comment for video000.mkv audio track 2"
        );
        assert_eq!(
            redacted["tracks"][1]["tags"]["comment"],
            "comment for video000.mkv subtitle track 3"
        );
    }

    #[test]
    fn redacts_nested_track_comment_without_tags_special_case() {
        let mut redactor = Redactor::default();
        let value = serde_json::json!({
            "filename": "The Movie (2026).mkv",
            "tracks": [{
                "index": 1,
                "track_type": "video",
                "metadata": {
                    "comment": "Private nested video note"
                }
            }]
        });

        let redacted = redactor.redact_json(value);

        assert_eq!(
            redacted["tracks"][0]["metadata"]["comment"],
            "comment for video000.mkv video track 1"
        );
    }

    #[test]
    fn file_context_registration_does_not_require_generic_text_redaction() {
        let mut redactor = Redactor::default();
        let value = serde_json::json!({
            "path": "/media/private/The Movie (2026).mkv",
            "tracks": [{
                "index": 1,
                "track_type": "video",
                "comment": "Private camera note"
            }]
        });

        let redacted = redactor.redact_json(value);
        let mappings = redactor.private_mappings();

        assert_eq!(redacted["path"], "/media/private/video000.mkv");
        assert_eq!(
            redacted["tracks"][0]["comment"],
            "comment for video000.mkv video track 1"
        );
        assert_eq!(
            mappings
                .iter()
                .filter(|mapping| mapping.replacement == "video000.mkv")
                .count(),
            1
        );
    }
}
