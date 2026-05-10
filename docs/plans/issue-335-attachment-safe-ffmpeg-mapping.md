# Issue 335: Attachment-Safe FFmpeg Mapping

Issue: <https://github.com/randomparity/voom/issues/335>
Branch: `fix/issue-335-preserve-mkv-attachments`

## Plan

1. Replace ffmpeg video-transcode `-map 0` usage with explicit mapping from
   VOOM's track inventory.
2. Exclude tracks classified as `attachment` from ffmpeg video-transcode output
   so image attachments cannot become video streams.
3. Preserve existing `-map 0` fallback for non-video-transcode paths and for
   files with no track inventory.
4. Add command-builder regression coverage, generated-corpus functional test
   steps, and an example policy documenting the behavior.

## Implementation

Video transcode commands now map each non-attachment track explicitly. This is
an intentional drop of attachment streams in the ffmpeg transcode path; VOOM's
attachment-management policies should be used when users need to preserve,
remove, or normalize attachments with mkvtoolnix.

## Functional Test

See `docs/functional-test-plan-issue-335.md`. It uses
`scripts/generate-test-corpus` to generate `attachment` and `cover-art`
fixtures, then runs `docs/examples/transcode-video-drop-attachments.voom`.

## Acceptance Criteria Review

Add a fixture with a Matroska attachment and run a video transcode:

- Covered by the generated-corpus functional plan.

Verify the output does not contain an extra PNG video stream:

- Covered by command-builder regression coverage and the functional test
  assertions.

Preserve attachments using correct ffmpeg mapping/metadata options, or
explicitly exclude attachments from video stream mapping and document policy
behavior:

- Implemented by explicitly excluding attachment tracks from ffmpeg video
  transcode mapping and documenting that attachment preservation should use the
  attachment-management/mkvtoolnix path.

## Adversarial Review

- Risk: dropping attachments is lossy. Mitigation: the behavior is explicit in
  docs and example policy comments, and it prevents silent attachment-to-video
  stream conversion.
- Risk: incomplete track inventory could omit streams. Mitigation: when no
  non-attachment tracks are available, command construction falls back to
  `-map 0`.
- Risk: this does not solve all metadata drift. Mitigation: stream metadata and
  disposition preservation remain tracked by issue #336.
