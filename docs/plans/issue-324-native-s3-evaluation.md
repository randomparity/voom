# Issue 324: Native S3 Backup Backend Evaluation

Issue: <https://github.com/randomparity/voom/issues/324>
Branch: `feat/issue-324-native-s3-eval`

## Decision

Do not implement native S3-compatible backup support yet. Keep S3 backup
destinations rclone-backed until there is a concrete user need that rclone
cannot satisfy.

The current backup backend uses one rclone adapter for upload, restore,
inventory verification, cleanup, retention-aware deletion, and health checks.
Adding native S3 now would introduce a second protocol implementation and a
second credential model without removing the existing rclone path.

## Current Stable Rust Options

Checked on 2026-05-10 with `cargo search` and `cargo info`.

`aws-sdk-s3`:

- Current version: `1.132.0`
- Rust version: `1.91.1`
- Notes: official AWS SDK, S3-specific, async Tokio runtime, SigV4/SigV4a
  support.

`object_store`:

- Current version: `0.13.2`
- Rust version: `1.85`
- Notes: generic object-store API with S3 support behind the `aws` feature.

`opendal`:

- Current version: `0.56.0`
- Rust version: `1.85`
- Notes: multi-service storage abstraction with S3 service and retry/timeout
  layers.

## Comparison

Dependencies:

- Rclone-backed S3 requires an external `rclone` binary and user-managed rclone
  config. It adds no Rust cloud SDK dependencies.
- Native Rust S3 adds SDK or object-store crates plus HTTP, TLS, signing, retry,
  and async runtime dependencies to the backup path.

Credential handling:

- Rclone-backed S3 keeps credentials outside VOOM in rclone config,
  environment, or provider credential stores. VOOM stores only destination names
  and remote paths.
- Native Rust S3 requires VOOM to define explicit credential sources, endpoint
  config, region handling, and redaction rules for every log and error path.

Multipart uploads:

- Rclone-backed S3 delegates multipart behavior to rclone, which already handles
  provider differences and large object uploads.
- Native Rust S3 requires VOOM to choose thresholds, part sizes, abort behavior,
  and provider compatibility rules, then test them.

Retries:

- Rclone-backed S3 delegates retries to rclone. VOOM sees success or failure
  from one subprocess command.
- Native Rust S3 requires VOOM to configure and expose retry semantics
  consistently across upload, download, verify, delete, and health probes.

Verification:

- Rclone-backed S3 verifies remote object size and compares SHA-256 when rclone
  can return the hash.
- Native Rust S3 can read object size and ETag, but checksum behavior varies by
  provider, upload mode, and object metadata. VOOM would still need explicit
  checksum metadata for reliable verification.

Compatibility:

- Rclone-backed S3 works for AWS S3 and S3-compatible providers that rclone
  supports, including provider-specific quirks.
- Native Rust S3 must be validated against AWS S3 plus MinIO or other
  S3-compatible services before it is safer than rclone.

## Implementation Gate

Native S3 should be reconsidered only when at least one of these becomes true:

1. Users need S3 backups on systems where installing rclone is not acceptable.
2. Users need S3-specific behavior that rclone cannot expose through the current
   model.
3. Backup operations need in-process progress, cancellation, or telemetry that a
   subprocess boundary cannot provide.

If native support is implemented later, the first reviewable slice should:

1. Add an explicit `kind = "native_s3"` destination instead of changing
   `kind = "s3"` semantics.
2. Keep credentials out of config by default, using environment or provider
   credential chains unless users explicitly request another source.
3. Store user-provided endpoint, bucket, region, key prefix, and storage class
   separately from credentials.
4. Add MinIO integration tests for upload, restore, verify, cleanup, and health
   checks before exposing the backend in docs.
5. Preserve rclone-backed S3 as the recommended backend for users who already
   rely on rclone remotes or non-AWS S3-compatible providers.

## Acceptance Criteria Review

Compare rclone-backed S3 with native Rust S3:

- Complete in this document for dependencies, credentials, multipart uploads,
  retries, and verification.

If native support is implemented:

- Native support is deferred. This document defines the required future
  constraints for explicit config and secret-safe logging.

Add integration tests against MinIO or equivalent local S3 service:

- Not applicable because native support is not implemented. Future
  implementation must include MinIO coverage before documentation advertises
  it.

Document when users should choose native S3 versus rclone-backed S3:

- Complete in `docs/remote-backups.md`. Today users should choose
  rclone-backed S3 because native S3 is not implemented.
