# Policy Testing

Policy tests run a `.voom` policy against JSON media fixtures and assert the
plans produced by the policy evaluator. They are fast, deterministic, and do
not need real media files, ffprobe, ffmpeg, mkvmerge, or a VOOM database.

Use policy tests when a policy is important enough that a future edit should
prove it still makes the same decisions. Good tests catch regressions in:

- phase ordering and `skip when` or `run_if` logic
- track filtering rules
- planned video and audio codec changes
- synthesized compatibility tracks
- warning-free happy paths

## Quick Start

Create a test suite next to your fixture:

```json
{
  "policy": "../minimal.voom",
  "cases": [
    {
      "name": "containerizes mp4",
      "fixture": "movie-mp4.json",
      "expect": {
        "phases_run": ["containerize"],
        "no_warnings": true
      }
    }
  ]
}
```

Run it:

```sh
cargo run -q -- policy test docs/examples/tests/minimal.test.json
```

Run every suite in a directory:

```sh
cargo run -q -- policy test docs/examples/tests
```

The runner discovers files ending in `.test.json`. Each suite can contain one
or more named cases. By default, fixture and policy paths are resolved relative
to the suite file.

## Suite Schema

A suite has two required fields:

```json
{
  "policy": "../movie-library.voom",
  "cases": []
}
```

`policy` is the policy file under test. `cases` is an array of fixture-backed
examples. A case has:

```json
{
  "name": "normalizes movie library tracks",
  "fixture": "rich-movie.json",
  "expect": {
    "phases_run": ["containerize", "normalize"]
  }
}
```

`name` should describe the behavior, not the implementation. `fixture` points
to the JSON media fixture. `expect` contains one or more assertions.

Use `--policy` to apply one policy file to every suite path:

```sh
cargo run -q -- policy test docs/examples/tests --policy docs/examples/minimal.voom
```

Use `--json` for machine-readable output:

```sh
cargo run -q -- policy test docs/examples/tests --json
```

## Fixture Schema

A fixture describes the media metadata the evaluator needs:

```json
{
  "path": "/media/movie.mp4",
  "container": "Mp4",
  "duration": 120.0,
  "size": 99,
  "tracks": [
    {
      "index": 0,
      "track_type": "Video",
      "codec": "h264",
      "language": "und",
      "title": "",
      "is_default": true,
      "is_forced": false,
      "channels": null,
      "channel_layout": null,
      "sample_rate": null,
      "bit_depth": null,
      "width": 1920,
      "height": 1080,
      "frame_rate": 23.976,
      "is_vfr": false,
      "is_hdr": false,
      "hdr_format": null,
      "pixel_format": null
    }
  ]
}
```

`container` uses the domain enum names, such as `Mkv`, `Mp4`, `Webm`, `Mov`,
`Avi`, `MpegTs`, and `Other`.

`track_type` uses the domain enum names:

- `Video`
- `AudioMain`, `AudioAlternate`, `AudioCommentary`, `AudioMusic`, `AudioSfx`,
  `AudioNonSpeech`
- `SubtitleMain`, `SubtitleForced`, `SubtitleCommentary`
- `Attachment`

Common fixture patterns:

- Use a small single-video fixture for container and transcode tests.
- Use a richer fixture with video, audio, subtitles, and attachments for track
  selection policies.
- Set commentary tracks to `AudioCommentary` or `SubtitleCommentary`.
- Set forced subtitles with `track_type: "SubtitleForced"` and
  `is_forced: true`.
- Model font attachments as `track_type: "Attachment"` with a font-like codec
  such as `ttf`, `otf`, or `ass`.

Fixtures can override executor capabilities. This is useful for policies that
plan transcodes or rely on `system.*` capability conditions:

```json
{
  "capabilities": {
    "executors": [
      {
        "name": "ffmpeg-executor",
        "decoders": ["h264", "hevc", "aac"],
        "encoders": ["hevc", "aac", "eac3"],
        "formats": ["matroska", "mp4"],
        "hw_accels": ["videotoolbox"]
      }
    ]
  }
}
```

A case can also provide `capabilities`; case-level capabilities override the
fixture-level block for that case only.

## Assertions

All assertions are optional, but each case should include at least one. Empty
expectations only prove that the policy can be evaluated.

`phases_run` requires every listed phase to produce a non-skipped plan:

```json
{
  "phases_run": ["containerize", "normalize"]
}
```

`phases_skipped` requires each listed phase to skip with a reason containing
the expected text:

```json
{
  "phases_skipped": {
    "transcode": "skip when"
  }
}
```

`audio_tracks_kept` checks how many audio tracks remain after planned removals:

```json
{
  "audio_tracks_kept": 2
}
```

`subtitle_tracks_kept` is the subtitle equivalent:

```json
{
  "subtitle_tracks_kept": 1
}
```

`audio_tracks_synthesized` counts planned audio synthesis actions:

```json
{
  "audio_tracks_synthesized": 1
}
```

`video_codec` checks the final planned video codec after transcode actions:

```json
{
  "video_codec": "hevc"
}
```

`no_warnings` fails if any plan contains evaluator warnings:

```json
{
  "no_warnings": true
}
```

## Snapshot Workflow

The CLI reserves `--update` for snapshot assertions, but snapshot assertions
are not available in this build. Running with `--update` currently exits with
an error instead of changing files.

Until snapshot mode lands, prefer explicit assertions. They are easier to
review than large plan snapshots and force each test to say which policy
decision matters.

When snapshot assertions are added, use this review etiquette:

- Update snapshots in a focused commit.
- Read every changed snapshot before committing.
- Treat broad snapshot churn as a policy behavior change, not test noise.
- Pair `--update` changes with a short explanation in the commit message or PR.

## Helper Commands

The active helper today is the test runner:

```sh
cargo run -q -- policy test <suite-or-directory>
```

The names `voom policy fixture extract` and `voom policy diff --fixture` are
reserved for fixture authoring and fixture-scoped policy diffs, but they are
not exposed by the current CLI. Do not put those commands in automation yet.

Until fixture extraction lands, author fixtures by copying an existing fixture
and changing only the fields relevant to the behavior under test. Keep fixtures
small enough that a reviewer can understand the scenario quickly.

Until fixture-scoped diffing lands, compare policy behavior by running the same
suite with a policy override:

```sh
cargo run -q -- policy test docs/examples/tests --policy path/to/candidate.voom
```

## CI Integration

Run policy tests after the workspace builds and Rust tests pass:

```yaml
- run: cargo build --workspace
- run: cargo test --workspace
- run: cargo run -q -- policy test docs/examples/tests
```

The repository also provides:

```sh
just policy-test-examples
```

`just ci` includes the example policy tests, so local CI and GitHub Actions
exercise the same policy-test suite.

## Writing Useful Tests

Start with one fixture and one assertion. Add more cases only when they cover a
different behavior: a skipped phase, a removed track, a warning path, or a
codec decision.

Prefer behavior names:

- good: `normalizes English movie tracks`
- good: `skips hevc transcode for existing hevc video`
- weak: `test case 1`

Avoid overfitting to implementation details. If the behavior is "foreign films
keep original-language audio", assert the resulting audio count or phase
execution, not the internal order of every unrelated phase.
