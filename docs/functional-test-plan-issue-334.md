# Functional Test Plan: Containerize Then Transcode File Identity

Issue: <https://github.com/randomparity/voom/issues/334>

## Goal

Verify that a file remuxed to a new container and then transcoded in the same
`voom process` run keeps the same persisted `files.id` for downstream plans.
The transcode phase must be able to persist `transcode_outcomes` without a
foreign-key failure.

## Corpus Setup

Generate a small corpus with MP4/H.264 sources that need both containerization
and video transcoding:

```sh
scripts/generate-test-corpus /tmp/voom-issue-334-corpus \
  --profile coverage \
  --only basic-h264-aac,letterbox-h264 \
  --duration 2 \
  --seed 334
```

Copy or reference `docs/examples/containerize-then-transcode.voom` as the test
policy.

## Execution

Run the process command with continuation enabled so failures are reported in
the session instead of stopping the batch:

```sh
voom process /tmp/voom-issue-334-corpus \
  --policy docs/examples/containerize-then-transcode.voom \
  --on-error continue \
  --workers 4
```

## Assertions

1. The process summary shows no `storage error: failed to insert transcode outcome`.
2. `voom report errors --session <session>` has no SQLite foreign-key failures.
3. `voom files list --path-prefix /tmp/voom-issue-334-corpus` shows one active
   row per generated source, now at the post-container path where applicable.
4. The transcode report/outcome query includes rows for files that were first
   containerized, proving the downstream `file_id` references an existing
   `files.id`.

## Adversarial Review

- The fix must not preserve stale path, hash, size, container, or tracks from
  the pre-container file; only the persisted row identity and row state should
  cross the re-introspection boundary.
- The fallback path where re-introspection fails already clones the previous
  file and therefore keeps the original ID; this test targets successful
  re-introspection, where a fresh UUID used to escape.
- A later storage-level refactor could also return the updated row from
  `record_post_execution`, but the pipeline still needs a local guard because
  it builds downstream plans before any follow-up DB read.
