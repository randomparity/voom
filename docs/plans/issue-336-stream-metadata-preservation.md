# Issue 336: Stream Metadata Preservation

Issue: <https://github.com/randomparity/voom/issues/336>
Branch: `fix/issue-336-preserve-stream-metadata`

## Plan

1. Make ffmpeg command construction explicitly preserve global metadata and
   chapters with `-map_metadata 0` and `-map_chapters 0`.
2. Compute the final stream metadata state from the source track inventory plus
   policy metadata/disposition actions.
3. Emit explicit per-output-stream `language`, `title`, and `default`/`forced`
   disposition arguments.
4. Add command-builder regression tests, an example policy, generated-corpus
   functional test steps, and an adversarial review.

## Implementation

FFmpeg commands now build a per-output-stream metadata table before the output
path is appended. The table starts from VOOM's source track inventory and then
applies policy actions such as `set_language`, `set_default`, and
`clear_forced`. The final values are emitted once, using output stream indexes,
so preservation does not fight explicit policy changes.

## Functional Test

See `docs/functional-test-plan-issue-336.md`. It uses
`scripts/generate-test-corpus` to generate multilingual, multi-subtitle, and
unknown-language fixtures and then processes them with
`docs/examples/metadata-stable-transcode.voom`.

## Acceptance Criteria Review

Add fixtures that include non-default subtitles, `und` language tags, and MP4
text subtitle inputs:

- Covered by the new `mp4-text-subtitle` generated-corpus fixture, existing
  multilingual corpus fixtures, and command-builder fixtures.

Verify transcode/containerize preserves dispositions and language metadata where
the output container supports them:

- Covered by ffmpeg command-builder regression tests that assert explicit
  metadata/disposition flags.

Where preservation is impossible, record/report the intentional change:

- Existing container compatibility safeguards continue to report incompatible
  surviving codecs before execution. This change does not bypass those checks.

## Adversarial Review

- Risk: explicit preservation could override policy metadata actions.
  Mitigation: final stream metadata is computed after applying policy actions.
- Risk: output stream indexes differ from input indexes after attachment
  exclusion. Mitigation: preservation uses the enumerated output stream order.
- Risk: forcing `title=` on streams with no title is noisy. Mitigation: it
  prevents stale or synthesized title metadata from leaking into outputs.
- Risk: mkvtoolnix structural remux paths may have separate metadata behavior.
  Mitigation: mkvtoolnix is already metadata-preserving for remux by default
  and its explicit metadata operations use mkvpropedit.
