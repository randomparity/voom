# Issue 323 Adversarial Review: Remote Backup Retention

Branch: `feat/issue-323-remote-retention`

## Acceptance Criteria Mapping

| Criterion | Status | Notes |
|-----------|--------|-------|
| Add destination retention config for minimum storage days | Implemented | `minimum_storage_days` is optional per destination. |
| Prevent cleanup from deleting remote backups younger than the configured minimum | Implemented | `voom backup cleanup --destination <name>` skips young inventory records. |
| Report skipped deletions with destination and age | Implemented | Cleanup prints destination, age, and configured minimum for each skip. |
| Tests cover local retention, remote minimum-age retention, and mixed destinations | Implemented | Unit tests cover local cleanup, young remote skips, and mixed eligible/skipped records. |
| Document provider examples such as Glacier Deep Archive | Implemented | Remote backup docs include an S3/Glacier-style minimum-age example. |

## Risks And Mitigations

1. Remote cleanup now mutates local inventory after successful deletion.
   - Mitigation: only successfully deleted records are removed; skipped and
     failed records remain for later verification or cleanup.

2. Inventory uses backup IDs to track successful deletes.
   - Mitigation: filtering is scoped to the selected destination, so records for
     other destinations remain untouched.

3. Providers can have lifecycle policies outside VOOM.
   - Mitigation: VOOM reports local decisions and relies on rclone delete
     success/failure for provider-side enforcement.
