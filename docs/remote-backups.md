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

## S3, SFTP, And WebDAV

Typed provider names document intent while keeping one execution model:

```toml
[[plugin.backup-manager.destinations]]
name = "archive-s3"
kind = "s3"
remote = "aws-archive:voom"

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

## Restore Status

`voom backup cleanup` continues to operate on local `.vbak` files. Remote
cleanup is tracked separately by retention work.

## Credential Safety

Keep credentials in rclone config, provider environment variables, or the
provider's native credential store. VOOM logs destination names and remote
object paths, not secrets.
