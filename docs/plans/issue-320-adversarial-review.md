# Issue 320 Remote Restore Adversarial Review

## Scope Reviewed

Implemented surface:

- `voom backup restore <file-path> --from <destination>` restores from remote
  inventory.
- `--output <path>` downloads to a separate path instead of replacing the
  original path.
- Restore checks current backup destination config, selects exactly one
  inventory record, verifies remote size through rclone, downloads to a
  same-directory temporary path, verifies local size, then renames into place.
- Ambiguous and missing inventory matches return clear errors.

## Acceptance Criteria Review

| Criterion | Status | Evidence / risk |
|---|---|---|
| Add `voom backup restore <file-path> --from <destination>` | Implemented | CLI parser tests and restore command branch. |
| Download to original path or explicit output path | Implemented | `--output` parser test and restore path handling. |
| Verify downloaded size before replacing content | Implemented | `download_with_rclone` checks remote size and downloaded local size before rename. |
| Refuse ambiguous restore requests | Implemented | `select_remote_restore_record_rejects_ambiguous_match`. |
| Test with fake rclone and generated corpus fixtures | Partial | Fake-rclone unit test covers download behavior. The generated-corpus workflow is documented for functional execution. |
| Document remote restore usage | Implemented | `docs/remote-backups.md`, CLI reference, and functional test plan updated. |

## Adversarial Findings

1. Restore selection intentionally fails when more than one inventory record
   matches an original path and destination. This prevents silent recovery of
   the wrong generation, but users need future tooling to choose a specific
   backup ID.

2. The implementation verifies byte size, not cryptographic hash. Issue #321
   covers deeper remote verification.

3. The temporary restore path lives beside the output path. That keeps rename
   atomic on the same filesystem, but it requires write permission in the target
   directory before restore can proceed.

4. Remote restore depends on the destination still being present in current
   config. Historical inventory alone is not enough to restore from a removed
   destination.
