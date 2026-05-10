# Issue 334: Preserve File ID After Containerize

Issue: <https://github.com/randomparity/voom/issues/334>
Branch: `fix/issue-334-preserve-post-exec-file-id`

## Plan

1. Reproduce the data flow with a multi-phase policy: `containerize` then
   `transcode-video`.
2. Preserve the persisted `files.id` when post-execution re-introspection
   returns a fresh `MediaFile`.
3. Keep re-introspected path, size, hash, container, tracks, and tags intact so
   downstream phases see the actual on-disk file.
4. Add focused regression coverage for the identity handoff.
5. Document a corpus-backed functional test and add an example policy.

## Implementation

`reintrospect_file` now normalizes successful re-introspection output through a
small helper that copies the persisted row identity and row state from the
pre-phase `MediaFile`. The file metadata discovered from disk remains sourced
from ffprobe.

## Functional Test

See `docs/functional-test-plan-issue-334.md`. It uses
`scripts/generate-test-corpus` to create H.264 content, then runs
`docs/examples/containerize-then-transcode.voom` to exercise a container path
change followed by a transcode outcome insert.

## Acceptance Criteria Review

Add a regression test for a multi-phase file that is containerized and then
transcoded in the same process run:

- Covered by the identity handoff regression and the documented functional
  corpus run. The test isolates the successful re-introspection path that
  produced the bad downstream ID.

Ensure re-introspection after a successful phase preserves the existing
`files.id` for downstream plans:

- Implemented by preserving `MediaFile.id` from the persisted pre-phase file
  after successful no-dispatch re-introspection.

Confirm `transcode_outcomes` inserts succeed after path/container changes:

- Covered by the functional test plan; the code-level change ensures the
  downstream transcode plan carries an existing `files.id`.

## Adversarial Review

- Risk: preserving the whole old `MediaFile` would hide real post-execution
  metadata. Mitigation: only `id`, `expected_hash`, and `status` are copied.
- Risk: storage and in-memory state could diverge if `record_post_execution`
  fails. Mitigation: the helper runs before the bundled write, but errors still
  abort the phase and surface with transition-recording context.
- Risk: a missing/fallback re-introspection path might need the same fix.
  Mitigation: fallback already clones the original file, so it already retains
  the persisted ID.
