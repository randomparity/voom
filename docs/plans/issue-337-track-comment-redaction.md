# Issue 337 Track Comment Redaction Plan

Issue: <https://github.com/randomparity/voom/issues/337>
Branch: `feat/issue-337-bug-report-cli`

## Problem

The current bug-report redactor handles filenames, paths, JSON strings, and
secrets. Track-level metadata can still contain private or identifying text in
comment fields on video, audio, or subtitle tracks. When such comments appear
in bug report output, they should be redacted with the same stable mapping
methodology.

Desired replacement shape:

```text
comment for video000.mkv video track 1
```

The replacement must be stable across `report.md`, `report.json`, events, jobs,
and redaction mapping files.

## Architecture

Extend `crates/voom-cli/src/commands/bug_report/redactor.rs` with a contextual
JSON pass for media-file-shaped objects. The redactor should detect a file
placeholder from `path` or `filename`, then redact track comment fields inside
that file's `tracks` array using the file placeholder, normalized track kind,
and track index.

Keep this scoped to bug-report output. Do not change domain storage, ffprobe
parsing, or media introspection in this follow-up unless a later issue asks to
persist additional track metadata that VOOM does not currently store.

## File Map

- Modify `crates/voom-cli/src/commands/bug_report/redactor.rs` for contextual
  track-comment redaction.
- Modify `crates/voom-cli/src/commands/bug_report/collect.rs` only if the new
  public redactor method needs a different call site.
- Modify `docs/functional-test-plan-issue-337.md` to include a track-comment
  redaction check.
- Modify `docs/plans/issue-337-bug-report-cli.md` to record the added scope.

## Redaction Rules

- Redact object fields named `comment` when they appear inside a track object.
- Also redact `tags.comment` and case variants such as `tags.COMMENT` when the
  track object carries nested tags.
- Only apply the contextual replacement when the containing object has enough
  file and track context:
  - file context from `path`, `filename`, or an already-redacted filename value;
  - track context from `track_type` or `type`;
  - track index from `index`.
- Replacement format:

```text
comment for <file-placeholder> <track-kind> track <index>
```

- Normalize track kind to `video`, `audio`, `subtitle`, or `attachment`.
  `audio_main`, `audio_commentary`, and other audio variants become `audio`.
  `subtitle_main`, `subtitle_forced`, and `subtitle_commentary` become
  `subtitle`.
- If a track comment appears without enough file context, fall back to the
  existing generic string redaction instead of inventing an ambiguous
  placeholder.
- Private mappings should contain the original comment value and replacement.
  Public mappings should contain only the replacement and kind.

## Task 1: Add Track Comment Redactor Tests

**Files:**
- Modify `crates/voom-cli/src/commands/bug_report/redactor.rs`

- [ ] **Step 1: Write failing contextual JSON tests**

Add tests inside the existing `#[cfg(test)] mod tests`:

```rust
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
    assert!(
        redactor
            .private_mappings()
            .iter()
            .any(|m| m.original == "Shot on a private camera"
                && m.replacement == "comment for video000.mkv video track 1")
    );
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
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```sh
cargo test -p voom-cli commands::bug_report::redactor::tests::redacts_video_track_comment_with_file_context
cargo test -p voom-cli commands::bug_report::redactor::tests::redacts_audio_and_subtitle_track_tag_comments
```

Expected: both tests fail because track comments are still emitted unchanged.

- [ ] **Step 3: Commit nothing yet**

Do not commit the failing tests by themselves unless pausing intentionally.

## Task 2: Implement Contextual Track Comment Redaction

**Files:**
- Modify `crates/voom-cli/src/commands/bug_report/redactor.rs`

- [ ] **Step 1: Add redaction kind**

Extend `RedactionKind`:

```rust
pub enum RedactionKind {
    FileName,
    Secret,
    PathComponent,
    TrackComment,
}
```

- [ ] **Step 2: Add contextual JSON traversal**

Refactor `redact_json` so object redaction can carry optional file context:

```rust
#[derive(Debug, Clone)]
struct FileContext {
    placeholder: String,
}

impl Redactor {
    pub fn redact_json(&mut self, value: serde_json::Value) -> serde_json::Value {
        self.redact_json_with_context(value, None)
    }

    fn redact_json_with_context(
        &mut self,
        value: serde_json::Value,
        file_context: Option<&FileContext>,
    ) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                self.redact_object_with_context(map, file_context)
            }
            serde_json::Value::Array(values) => serde_json::Value::Array(
                values
                    .into_iter()
                    .map(|value| self.redact_json_with_context(value, file_context))
                    .collect(),
            ),
            serde_json::Value::String(s) => serde_json::Value::String(self.redact_text(&s)),
            other => other,
        }
    }
}
```

Implement `redact_object_with_context` so it:

1. Detects file context from `path` or `filename` before redacting child tracks.
2. Redacts normal string values with existing `redact_text`.
3. Calls a track-specific helper for objects inside `tracks`.
4. Preserves all keys and non-comment values.

- [ ] **Step 3: Add file context detection helper**

Add:

```rust
fn file_context_from_map(&mut self, map: &serde_json::Map<String, serde_json::Value>)
    -> Option<FileContext>
{
    let source = map
        .get("filename")
        .and_then(serde_json::Value::as_str)
        .or_else(|| map.get("path").and_then(serde_json::Value::as_str))?;
    let redacted = self.redact_text(source);
    let placeholder = redacted
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(redacted.as_str())
        .to_string();
    if placeholder == source {
        return None;
    }
    Some(FileContext { placeholder })
}
```

- [ ] **Step 4: Add track comment replacement helper**

Add:

```rust
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
```

When replacing a comment value, call:

```rust
self.register_replacement(
    original_comment.to_string(),
    replacement.clone(),
    RedactionKind::TrackComment,
);
```

Then write the replacement into the output JSON.

- [ ] **Step 5: Run tests to verify GREEN**

Run:

```sh
cargo test -p voom-cli commands::bug_report::redactor::tests
```

Expected: all redactor tests pass.

- [ ] **Step 6: Run formatting**

Run:

```sh
cargo fmt --check
```

Expected: pass.

- [ ] **Step 7: Commit**

```sh
git add crates/voom-cli/src/commands/bug_report/redactor.rs
git commit -m "feat(cli): redact bug report track comments"
```

## Task 3: Verify Report Output Does Not Leak Track Comments

**Files:**
- Modify `crates/voom-cli/src/commands/bug_report/render.rs`
- Modify `docs/functional-test-plan-issue-337.md`

- [ ] **Step 1: Add render regression test**

Add a second render test in `render.rs` that builds a bundle containing a
track comment in `storage` and verifies `report.md` and `report.json` use the
replacement:

```rust
#[test]
fn write_bundle_excludes_original_track_comment() {
    let dir = tempfile::tempdir().unwrap();
    let mut redactor = Redactor::default();
    let storage = redactor.redact_json(serde_json::json!({
        "path": "/media/The Movie (2026).mkv",
        "tracks": [{
            "index": 1,
            "track_type": "video",
            "comment": "Private track comment"
        }]
    }));
    let mut bundle = test_bundle(dir.path());
    bundle.storage = StorageCapture::Available {
        table_row_counts: Vec::new(),
        jobs: Vec::new(),
        events: vec![storage],
        health_checks: Vec::new(),
    };
    bundle.redactions = redactor.report();
    bundle.private_redactions = redactor.private_mappings();

    write_bundle(&bundle).unwrap();

    let report = std::fs::read_to_string(dir.path().join("report.md")).unwrap();
    let data = std::fs::read_to_string(dir.path().join("report.json")).unwrap();
    let private = std::fs::read_to_string(dir.path().join("redactions.local.json")).unwrap();

    assert!(report.contains("comment for video000.mkv video track 1"));
    assert!(data.contains("comment for video000.mkv video track 1"));
    assert!(!report.contains("Private track comment"));
    assert!(!data.contains("Private track comment"));
    assert!(private.contains("Private track comment"));
}
```

- [ ] **Step 2: Run render tests**

Run:

```sh
cargo test -p voom-cli commands::bug_report::render::tests
```

Expected: render tests pass.

- [ ] **Step 3: Update functional test plan**

Add this setup line to `docs/functional-test-plan-issue-337.md`:

```sh
# In a real media fixture, set a video/audio/subtitle track COMMENT tag such as
# "Private track comment" before running `voom scan`.
```

Add these checks:

```sh
rg "comment for video000\\.mkv (video|audio|subtitle) track [0-9]+" \
  /tmp/voom-issue-337-report/report.md \
  /tmp/voom-issue-337-report/report.json

! rg "Private track comment" \
  /tmp/voom-issue-337-report/report.md \
  /tmp/voom-issue-337-report/report.json
```

- [ ] **Step 4: Run docs-related test**

Run:

```sh
cargo test -p voom-cli commands::bug_report::render::tests
```

Expected: pass.

- [ ] **Step 5: Commit**

```sh
git add crates/voom-cli/src/commands/bug_report/render.rs docs/functional-test-plan-issue-337.md
git commit -m "test(cli): cover bug report track comment output"
```

## Task 4: Final Verification

**Files:**
- All files touched above.

- [ ] **Step 1: Run focused tests**

Run:

```sh
cargo test -p voom-cli commands::bug_report
cargo test -p voom-cli cli::tests::test_bug_report_
```

Expected: all pass.

- [ ] **Step 2: Run lint and format**

Run:

```sh
cargo fmt --check
cargo clippy -p voom-cli --all-targets --all-features -- -D warnings
```

Expected: both pass with no warnings.

- [ ] **Step 3: Smoke-test generation**

Run:

```sh
trash /tmp/voom-issue-337-track-comment-smoke 2>/dev/null || true
cargo run -p voom-cli -- bug-report generate \
  --out /tmp/voom-issue-337-track-comment-smoke
```

Expected:

- `report.md` exists.
- `report.json` exists.
- `redactions.local.json` exists.
- command output tells the user to review report files before upload.

- [ ] **Step 4: Commit fixes if verification changes code**

If clippy or formatting requires a change, commit it separately:

```sh
git add crates/voom-cli/src/commands/bug_report docs/functional-test-plan-issue-337.md
git commit -m "fix(cli): satisfy track comment redaction checks"
```

## Self-Review

- Spec coverage: the plan redacts track comment fields in bug-report JSON and
  rendered output, uses stable placeholders, and records originals only in the
  private local redaction map.
- Placeholder scan: no unresolved TBD/TODO/fill-in markers are present.
- Type consistency: the plan uses existing `Redactor::redact_json`,
  `Redactor::private_mappings`, `RedactionKind`, `StorageCapture`, and
  `write_bundle` names from the current branch.
