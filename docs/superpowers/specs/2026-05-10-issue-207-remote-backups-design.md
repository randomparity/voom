# Issue 207 Remote Backups Design

**Goal:** Add remote backup destinations so destructive processing is blocked until
configured offsite backups are uploaded and verified.

**Issue:** https://github.com/randomparity/voom/issues/207

## Scope

This implementation ships rclone-backed remote destinations through the existing
native `backup-manager` plugin. The existing local sibling/global backup remains
the hot path and mandatory staging object. Remote destinations receive a copy of
the original source file before executor plugins can mutate it.

The feature supports these configured destination kinds:

- `local`: existing filesystem backup behavior.
- `rclone`: any rclone remote or local test target accepted by `rclone copyto`.
- `s3`, `sftp`, `webdav`: typed aliases that use rclone remotes. Users configure
  the provider in rclone and reference it from VOOM.

Direct AWS SDK S3, native SFTP, native WebDAV, restic, persistent DB tracking,
and cloud-cost retention rules are intentionally not part of this first
implementation because they multiply credentials, protocol behavior, dependency
surface, and schema work. The rclone path still covers the requested providers
end-to-end while keeping one execution and verification model.

## User Configuration

Remote destinations live in `~/.config/voom/config.toml` under
`[plugin.backup-manager]`:

```toml
[plugin.backup-manager]
use_global_dir = true
backup_dir = "/var/backups/voom"
verify_after_upload = true
block_on_remote_failure = true

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "b2:voom-backups"
bandwidth_limit = "10M"

[[plugin.backup-manager.destinations]]
name = "archive-s3"
kind = "s3"
remote = "aws-archive:voom-backups"
```

`block_on_remote_failure` defaults to `true`. When true, any failed upload or
verification returns an error from `PlanExecuting`, so destructive executors do
not receive `PlanCreated`. Credentials stay in rclone or environment-specific
provider configuration; VOOM only logs destination names and remote paths.

Policy files continue to opt into retained backups with:

```voom
config {
  keep_backups: true
}
```

The policy examples demonstrate destructive policies that rely on remote backup
configuration but do not embed credentials or remote endpoints in `.voom` files.

## Remote Path Layout

For each backup record, `backup-manager` computes one backup UUID and local
backup path. Remote destinations use deterministic object names:

```text
<remote>/<uuid>/<original-file-name>.vbak
```

The UUID avoids collisions across files with the same name. The sanitized file
name keeps restore/list output understandable.

## Upload And Verify Flow

1. Reject symlink source files.
2. Read source metadata and allocate one backup UUID.
3. Create and verify the local `.vbak` staging backup.
4. For each configured remote destination, upload the original source file to
   the destination path before mutation.
5. If `verify_after_upload` is true, verify uploaded byte size matches the
   source size.
6. Return the backup record only after all required destinations succeed.

The rclone backend runs:

```sh
rclone copyto <source> <remote-path> --bwlimit <limit>
rclone lsf --format s <remote-parent>
```

Verification fails if the expected object size is absent or differs from the
source size. Tests use fake command runners instead of invoking real rclone.

## Restore And Listing

The existing `voom backup list`, `restore`, and `cleanup` commands keep their
local `.vbak` behavior. The initial remote implementation adds metadata to the
backup record and event results so future persistent listing can be built on the
same model. Remote restore from persistent history is not exposed until backup
records survive process exit.

## Health Checks

The plugin validates remote destination configuration during `init()`:

- destination names are non-empty and unique,
- destination kind is supported,
- rclone-backed destinations include a remote,
- bandwidth limits contain no shell metacharacter-sensitive behavior because
  subprocess execution passes argv directly.

Runtime connectivity is verified during upload. A later health-check extension
can publish `HealthStatus` events without changing the destination model.

## Test Strategy

Unit tests cover configuration parsing, destination validation, local backup
compatibility, rclone argument construction, upload failure blocking, size
verification failure, multiple destinations, and credential redaction in error
paths.

Functional tests generate disposable media through `scripts/generate-test-corpus`
and configure a fake rclone executable that copies to a local directory. This
exercises real `voom process` backup dispatch without relying on cloud services.

## Documentation And Examples

User-facing docs go in `docs/remote-backups.md`, with `docs/cli-reference.md`
linking from `voom backup`. Example policy and TOML files go under
`docs/examples/`:

- `remote-backup-transcode.voom`
- `remote-backup-rclone.toml`
- `remote-backup-s3.toml`

## Adversarial Review Notes

The riskiest failure mode is claiming S3/SFTP/WebDAV support without testing
real cloud services. The implementation avoids protocol-specific promises by
documenting them as rclone-backed providers and testing command construction,
blocking behavior, and local fake-rclone end-to-end behavior.

The second risk is leaking secrets. VOOM does not accept inline credential
values for remote destinations in this implementation, and all errors mention
only destination names plus sanitized remote paths.

The third risk is false safety if remote uploads fail but local backups succeed.
`block_on_remote_failure = true` is the default and is covered by tests that
prove the backup operation returns an error before executor dispatch can happen.
