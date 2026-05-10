# Remote Backups Functional Test Plan

This plan verifies issue 207 behavior without cloud credentials by using
`scripts/generate-test-corpus` and a fake `rclone` executable that copies remote
objects into a local directory.

## Generate Corpus

```sh
rm -rf /tmp/voom-remote-backup-corpus
scripts/generate-test-corpus /tmp/voom-remote-backup-corpus \
  --profile smoke \
  --duration 1
```

Expected: `manifest.json` exists and at least one generated media file is
available for processing.

## Create Fake Rclone

```sh
rm -rf /tmp/voom-remote-backup-bin /tmp/voom-remote-backup-target
mkdir -p /tmp/voom-remote-backup-bin /tmp/voom-remote-backup-target
cat > /tmp/voom-remote-backup-bin/rclone <<'SH'
#!/usr/bin/env bash
set -euo pipefail

cmd="$1"
shift

case "$cmd" in
  copyto)
    source="$1"
    remote="$2"
    target="/tmp/voom-remote-backup-target/${remote//:/_}"
    mkdir -p "$(dirname "$target")"
    cp "$source" "$target"
    ;;
  size)
    shift
    remote="$1"
    target="/tmp/voom-remote-backup-target/${remote//:/_}"
    bytes="$(wc -c < "$target" | tr -d ' ')"
    printf '{"bytes":%s,"count":1}\n' "$bytes"
    ;;
  *)
    echo "unsupported fake rclone command: $cmd" >&2
    exit 64
    ;;
esac
SH
chmod +x /tmp/voom-remote-backup-bin/rclone
```

Expected: `/tmp/voom-remote-backup-bin/rclone` is executable.

## Configure VOOM

```sh
rm -rf /tmp/voom-remote-config
mkdir -p /tmp/voom-remote-config/voom
cat > /tmp/voom-remote-config/voom/config.toml <<'TOML'
data_dir = "/tmp/voom-remote-config/voom/data"

[plugin.backup-manager]
use_global_dir = true
backup_dir = "/tmp/voom-remote-config/voom/local-backups"
verify_after_upload = true
block_on_remote_failure = true
rclone_path = "/tmp/voom-remote-backup-bin/rclone"

[[plugin.backup-manager.destinations]]
name = "fake-offsite"
kind = "rclone"
remote = "fake:voom"
TOML
```

Expected: config contains one rclone-backed destination.

## Process Corpus

```sh
XDG_CONFIG_HOME=/tmp/voom-remote-config cargo run -p voom-cli -- process \
  /tmp/voom-remote-backup-corpus \
  --policy docs/examples/remote-backup-transcode.voom \
  --yes
```

Expected: processing succeeds, local `.vbak` files are retained because the
policy sets `keep_backups: true`, `/tmp/voom-remote-backup-target` contains
remote backup copies under `fake_voom/<uuid>/`, and the remote inventory file
exists at:

```text
/tmp/voom-remote-config/voom/data/backup-manager/remote-backups.jsonl
```

List the remote inventory:

```sh
XDG_CONFIG_HOME=/tmp/voom-remote-config cargo run -p voom-cli -- backup list \
  --destination fake-offsite \
  --format json
```

Expected: JSON output contains at least one record with
`"destination_name": "fake-offsite"` and `"status": "verified"`.

Restore to an explicit output path:

```sh
first_file="/tmp/voom-remote-backup-corpus/$(jq -r '.generated[0].filename' /tmp/voom-remote-backup-corpus/manifest.json)"
XDG_CONFIG_HOME=/tmp/voom-remote-config cargo run -p voom-cli -- backup restore \
  "$first_file" \
  --from fake-offsite \
  --output /tmp/voom-remote-restored.mkv \
  --yes
```

Expected: `/tmp/voom-remote-restored.mkv` exists and has the same byte size as
the inventory record selected for `fake-offsite`.

## Failure Blocks Destructive Work

```sh
cat > /tmp/voom-remote-backup-bin/rclone <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "simulated remote failure" >&2
exit 50
SH
chmod +x /tmp/voom-remote-backup-bin/rclone

XDG_CONFIG_HOME=/tmp/voom-remote-config cargo run -p voom-cli -- process \
  /tmp/voom-remote-backup-corpus \
  --policy docs/examples/remote-backup-transcode.voom \
  --yes
```

Expected: processing exits non-zero with a backup-manager error before executor
plugins modify files.
