# Functional Test Plan: Attachment-Safe Video Transcode Mapping

Issue: <https://github.com/randomparity/voom/issues/335>

## Goal

Verify that an ffmpeg video transcode does not map Matroska attachments into
the output as PNG/JPEG video streams.

## Corpus Setup

Generate a fixture with font and image attachments:

```sh
scripts/generate-test-corpus /tmp/voom-issue-335-corpus \
  --profile coverage \
  --only attachment,cover-art \
  --duration 2 \
  --seed 335
```

Use `docs/examples/transcode-video-drop-attachments.voom` as the policy.

## Execution

```sh
voom process /tmp/voom-issue-335-corpus \
  --policy docs/examples/transcode-video-drop-attachments.voom \
  --on-error continue \
  --workers 2
```

## Assertions

1. `ffprobe` post-run output contains no extra PNG/JPEG video stream created
   from an attachment.
2. `voom inspect <file> --format json` does not show attachment images
   reclassified as `Video` tracks.
3. If attachment preservation is required, run an explicit mkvtoolnix-backed
   attachment policy before or after the ffmpeg transcode instead of relying on
   ffmpeg stream-copy behavior.

## Adversarial Review

- The ffmpeg transcode path should not use `-map 0` when attachments are known
  in the VOOM track list.
- Audio and subtitle tracks must still be mapped explicitly.
- Files with no track inventory should fall back to `-map 0` rather than
  producing an empty output mapping.
