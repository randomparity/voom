# Remote Backup Destinations

VOOM can upload backups to rclone-backed remote destinations before destructive
processing starts. The local `.vbak` backup is still created first; remote
uploads are an additional safety check before executor plugins can modify the
source file.

Remote backup configuration lives in `~/.config/voom/config.toml`:

```toml
[plugin.backup-manager]
use_global_dir = true
backup_dir = "/var/backups/voom"
verify_after_upload = true
block_on_remote_failure = true
rclone_path = "rclone"

[[plugin.backup-manager.destinations]]
name = "offsite"
kind = "rclone"
remote = "b2:voom-backups"
bandwidth_limit = "10M"
minimum_storage_days = 30
```

`kind` may be `rclone`, `s3`, `sftp`, or `webdav`. All remote kinds are backed
by rclone in this implementation; configure credentials and provider details in
rclone, then reference the remote from VOOM. VOOM does not accept inline access
keys or passwords for backup destinations.

## Behavior

For every destructive plan, `backup-manager`:

1. Creates the normal local `.vbak` backup.
2. Uploads the original source file to each remote destination.
3. Verifies the remote object size when `verify_after_upload = true`.
4. Blocks the destructive operation if a required upload or verification fails.

`block_on_remote_failure` defaults to `true`. Set it to `false` only when local
backups are acceptable for that run and remote upload failures should be logged
without blocking processing.

Remote backup paths use this layout:

```text
<remote>/<backup-uuid>/<original-file-name>.vbak
```

The UUID prevents collisions when different source directories contain files
with the same name.

## Remote Inventory

Every successful remote upload appends a JSONL record under the VOOM data
directory:

```text
<data_dir>/backup-manager/remote-backups.jsonl
```

List records for one destination:

```sh
voom backup list --destination offsite
voom backup list --destination offsite --format json
```

Verify the recorded inventory against the remote destination:

```sh
voom backup verify --destination offsite
voom backup verify --destination offsite --format json
```

Remote verification checks object size for every inventory record. New
inventory records also include a SHA-256 of the backup content; when rclone can
return SHA-256 metadata for the remote object, VOOM compares that hash as well.
The command reports `verified`, `missing`, `size_mismatch`, `hash_mismatch`, or
`error` for each record and exits non-zero when any record is not verified.

Restore a remote backup from one destination:

```sh
voom backup restore /media/movies/film.mkv --from offsite
```

Download without replacing the original:

```sh
voom backup restore /media/movies/film.mkv --from offsite --output /tmp/film-restored.mkv
```

Remote restore refuses ambiguous requests when more than one inventory record
matches the same original path and destination. Use a local `.vbak` restore or
prune/adjust the inventory when you need to recover a specific historical
version.

Local backup listing still scans `.vbak` files by path:

```sh
voom backup list /media/movies
```

Remote cleanup deletes inventory records from one configured destination:

```sh
voom backup cleanup --destination offsite
voom backup cleanup --destination offsite --yes
```

`minimum_storage_days` protects young remote objects from cleanup. Skipped
deletions are reported with the destination and object age. Set this for
providers with minimum billable storage windows, such as Amazon S3 Glacier Deep
Archive, where early deletion can incur charges. Objects deleted successfully
are removed from the local remote-backup inventory; skipped and failed records
remain.

## S3, SFTP, And WebDAV

Typed provider names document intent while keeping one execution model:

```toml
[[plugin.backup-manager.destinations]]
name = "archive-s3"
kind = "s3"
remote = "aws-archive:voom"
minimum_storage_days = 180

[[plugin.backup-manager.destinations]]
name = "nas-sftp"
kind = "sftp"
remote = "vps:/srv/voom"

[[plugin.backup-manager.destinations]]
name = "library-webdav"
kind = "webdav"
remote = "dav:voom"
```

Create and test those remotes with rclone first:

```sh
rclone config
rclone lsd aws-archive:
rclone lsd vps:
rclone lsd dav:
```

### Native S3 Backend

VOOM does not currently ship a native S3 backend. Use `kind = "s3"` with an
rclone remote for AWS S3 and S3-compatible providers.

Prefer rclone-backed S3 when:

1. You already have rclone remotes configured.
2. You use S3-compatible storage with provider-specific endpoint behavior.
3. You need the same backup behavior across S3, SFTP, WebDAV, and other rclone
   remotes.
4. You want credentials to stay in rclone config, environment variables, or the
   provider's native credential store instead of VOOM config.

A native S3 backend should be added only if users need an in-process S3 client,
cannot install rclone, or need S3-specific behavior that rclone-backed
destinations cannot provide. If that backend is added later, it should use a
separate destination kind instead of changing the behavior of `kind = "s3"`.

## Health Checks

`voom env check` and the deprecated `voom health check` alias report one
`backup_destination:<name>` health check for every configured backup
destination. For rclone-backed destinations, VOOM verifies:

1. Backup destination config parses and validates.
2. The configured `rclone_path` can execute.
3. The remote can be listed.
4. A small probe object can be written and deleted.

Failures are reported as:

| Status | Meaning | Typical fix |
|--------|---------|-------------|
| `config_invalid` | Destination config is malformed, incomplete, or has duplicate names | Fix `[plugin.backup-manager]` in `config.toml` |
| `rclone_unavailable` | The configured rclone executable cannot run | Install rclone or correct `rclone_path` |
| `remote_unreachable` | Rclone can run but cannot list the remote | Check rclone credentials, provider availability, and the remote path |
| `probe_failed` | Listing succeeded but the write/delete probe failed | Check write/delete permissions and quota |

Health output names the destination and kind only. It does not print remote URLs
or credential-bearing configuration values.

## Credential Safety

Keep credentials in rclone config, provider environment variables, or the
provider's native credential store. VOOM logs destination names and remote
object paths, not secrets.
