# Issue 207 Remote Backups Adversarial Review

## Scope Reviewed

Branch: `feat/issue-207-remote-backups`

Implemented surface:

- `backup-manager` loads remote destinations from `[plugin.backup-manager]`.
- `rclone`, `s3`, `sftp`, and `webdav` destination kinds are accepted.
- All remote destination kinds are rclone-backed.
- Local `.vbak` creation remains the staging and restore path.
- Remote uploads run before `PlanExecuting` returns success.
- Remote failures block destructive processing by default.
- Remote object size verification runs through `rclone size --json`.
- Backup event results include local and remote backup metadata.
- User docs, example policy/config files, and a generated-corpus functional test
  plan document the feature.

## Acceptance Criteria Review

| Issue 207 criterion | Status | Evidence / risk |
|---|---|---|
| S3-compatible destinations work end-to-end | Partial | `kind = "s3"` is supported through rclone remotes. Native S3 SDK upload/list/restore/delete is not implemented. |
| Rclone destinations work | Implemented for upload and size verification | `BackupDestinationConfig::rclone`, `RcloneCommand::copyto`, `upload_with_rclone`, and backup unit tests cover command construction and blocking behavior. |
| SFTP and WebDAV work | Partial | `kind = "sftp"` and `kind = "webdav"` are accepted as rclone-backed aliases. Real provider integration depends on rclone config. |
| Pre-modification upload-and-verify is enforced | Implemented | `backup-manager` runs on `PlanExecuting` before executor dispatch; upload errors return from `backup_file`. |
| Failed uploads block destructive ops by default | Implemented | `block_on_remote_failure` defaults to true and `remote_upload_failure_blocks_backup` covers it. |
| Bandwidth limits respected | Implemented for rclone | `RcloneCommand::copyto` passes `--bwlimit` as argv, not through a shell. |
| `voom backup restore --from <dest>` works | Not implemented | Existing restore remains local `.vbak` only. |
| `voom backup verify --destination` detects bit-rot | Not implemented | Upload-time size verification exists; standalone remote verify CLI does not. |
| Retention policy honors cost-aware minimums | Not implemented | No cloud retention implementation or schema exists in this branch. |
| Health check covers all configured destinations | Not implemented | Config validation exists; `voom health check` integration is absent. |
| Credentials never logged in plaintext | Implemented by design constraint | VOOM does not accept inline remote credentials. Error paths log destination names and remote paths only. |

## Adversarial Findings

1. The branch is honest rclone-backed support, not native cloud protocol support.
   The docs must keep saying S3/SFTP/WebDAV require rclone remotes. Marketing
   this as native S3 would be misleading.

2. Remote restore is the biggest user-facing gap. Upload safety is useful, but
   users cannot yet ask VOOM to pull a specific remote backup back down.

3. Backup status is still process-local. `BackupRecord.remote_backups` and event
   JSON expose remote metadata during a run, but there is no persistent backup
   inventory table yet.

4. `rclone size --json <remote-object>` assumes rclone returns a JSON object
   with `bytes`. This should be validated against at least one real rclone
   remote before broad release notes claim provider-level coverage.

5. `block_on_remote_failure = false` can create a false sense of offsite safety.
   The docs call it out, but CLI output could be more visible in a future
   change.

6. The fake-rclone functional test plan verifies dispatch and local subprocess
   behavior, not cloud authentication, provider permissions, lifecycle rules, or
   minimum-storage billing behavior.

## Recommended Follow-Up Issues

The following work is still needed for the full issue 207 acceptance list:

- Remote backup inventory and `voom backup list --destination`.
- Remote restore through `voom backup restore <file-path> --from <destination>`.
- Standalone remote verification through `voom backup verify --destination`.
- `voom health check` coverage for backup destinations.
- Cost-aware retention guards for providers with minimum storage durations.
- Optional native S3 backend if direct AWS SDK support remains desired after
  rclone-backed S3 is available.
