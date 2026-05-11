# VOOM CLI Reference

## Global Options

```
voom [OPTIONS] <COMMAND>
```

| Option | Description |
|--------|-------------|
| `-v`, `--verbose` | Increase verbosity. `-v` = info, `-vv` = debug, `-vvv` = trace |
| `-q`, `--quiet` | Suppress progress bars and status messages |
| `-y`, `--yes` | Assume "yes" to all confirmation prompts (for automation) |
| `--force` | Skip the process lock (use if a previous run crashed and left a stale lock) |
| `--version` | Print version |
| `--help` | Print help |

Verbosity can also be controlled via the `RUST_LOG` environment variable (e.g., `RUST_LOG=debug`).

Output formats accept `table`, `json`, `plain`, or `csv` where applicable.

---

## Commands

### `voom scan`

Discover and introspect media files in one or more directories.

```
voom scan <PATH>... [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<PATH>...` | *required* | Directories to scan for media files (one or more) |
| `-r`, `--recursive` | `true` | Recurse into subdirectories |
| `-w`, `--workers <N>` | `0` (auto) | Number of parallel workers for hashing |
| `--no-hash` | `false` | Skip content hashing (faster scans) |
| `-f`, `--format <FORMAT>` | *none* | Output format (omit for summary only) |

Before scanning, stale database entries for files that no longer exist under the scanned directory are automatically pruned (along with their associated plans and processing stats).

The scanner walks the directory tree (using rayon for parallelism), identifies media files by extension, computes xxHash64 content hashes (unless `--no-hash`), and runs ffprobe for metadata extraction. Results are stored in the SQLite database. Files that fail introspection are recorded as "bad files" for tracking and can be reviewed with `voom db list-bad`.

**Examples:**

```bash
voom scan /media/movies -r
voom scan /media/tv --workers 8 --no-hash
voom scan /media/movies /media/tv --format table
```

---

### `voom inspect`

Show detailed metadata for a media file.

```
voom inspect <FILE> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<FILE>` | *required* | Media file to inspect |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |
| `--tracks-only` | `false` | Show only track information |
| `--history` | `false` | Include file transition history (table and json formats) |

**Examples:**

```bash
voom inspect movie.mkv
voom inspect movie.mkv --format json
voom inspect movie.mkv --tracks-only
voom inspect movie.mkv --history
```

---

### `voom process`

Apply a policy to media files.

```
voom process <PATH>... [--policy <FILE> | --policy-map <TOML>] [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<PATH>...` | *required* | Directories or files to process (one or more) |
| `-p`, `--policy <FILE>` | *optional* | Policy file (`.voom`) to apply to all files (conflicts with `--policy-map`) |
| `--policy-map <TOML>` | *optional* | TOML file mapping directory prefixes to policies (conflicts with `--policy`) |
| `--dry-run` | `false` | Show what would be done without making changes |
| `--estimate` | `false` | Estimate runtime and output size without executing plans |
| `--estimate-only` | `false` | Alias for `--estimate` |
| `--on-error <STRATEGY>` | `fail` | Error handling: `continue` or `fail` |
| `-w`, `--workers <N>` | `0` (auto) | Number of parallel workers |
| `--approve` | `false` | Require interactive approval for each file |
| `--no-backup` | `false` | Skip creating backups before modifications |
| `--force-rescan` | `false` | Re-attempt introspection on previously failed files |
| `--flag-size-increase` | `false` | Tag files whose output is larger than the original |
| `--flag-duration-shrink` | `false` | Flag files whose output duration is >5% shorter than the original (post-execution) |
| `--plan-only` | `false` | Output raw plans as JSON to stdout without executing (implies --dry-run) |
| `--confirm-savings <SIZE>` | *optional* | Execute only files whose estimated savings meet the per-file threshold |
| `--priority-by-date` | `false` | Assign job priority based on file modification date |

Before processing, stale database entries for files that no longer exist under the target directory are automatically pruned (along with their associated plans and processing stats). Files that previously failed introspection (tracked as "bad files") are automatically skipped unless `--force-rescan` is set.

Error handling strategies:
- **`fail`** — Stop processing the batch on the first file job error.
- **`continue`** — Continue processing remaining files after a file job error.

If an executable plan fails, the file job is marked failed and the final
summary error count is non-zero. With `--on-error continue`, the command still
finishes the batch; inspect `voom jobs list --status failed` and
`voom report errors --session <session>` for details.

**Examples:**

```bash
# Dry run to preview changes
voom process /media/movies --policy normalize.voom --dry-run

# Process multiple directories with 4 workers
voom process /media/movies /media/tv --policy normalize.voom --workers 4

# Process with a policy map
voom process /media --policy-map policies.toml

# Output plans as JSON without executing
voom process /media/movies --policy normalize.voom --plan-only

# Estimate cost without executing
voom process /media/movies --policy normalize.voom --estimate

# Only execute plans estimated to save at least 1 GB per file
voom process /media/movies --policy normalize.voom --confirm-savings 1GB
```

---

### `voom estimate`

Estimate policy cost without modifying files.

```bash
voom estimate /media/movies --policy normalize.voom --workers 4
voom estimate calibrate
voom estimate calibrate --benchmark-corpus /tmp/voom-estimate-corpus --max-fixtures 3
```

The standalone command shares the `process --estimate` planning path. Calibration
records local codec/backend samples used by later estimates. `--benchmark-corpus`
uses media from `scripts/generate-test-corpus` and persists measured HEVC
software samples with a holdout estimate-vs-actual summary; without it,
calibration seeds conservative default samples.

---

### `voom policy`

Policy file management.

#### `voom policy list`

List all loaded policies.

```bash
voom policy list
```

#### `voom policy validate`

Validate a policy file for syntax and semantic errors.

```
voom policy validate <FILE>
```

Reports errors with source locations and suggestions (e.g., "Unknown codec 'h256'. Did you mean 'h265'?").

#### `voom policy show`

Show the compiled form of a policy.

```
voom policy show <FILE>
```

#### `voom policy format`

Auto-format a policy file in place.

```
voom policy format <FILE>
```

#### `voom policy diff`

Compare two compiled policies side by side.

```
voom policy diff <A> <B>
```

**Examples:**

```bash
voom policy validate my-policy.voom
voom policy format my-policy.voom
voom policy show my-policy.voom
voom policy diff old-policy.voom new-policy.voom
```

---

### `voom plugin`

Plugin management.

#### `voom plugin list`

List all registered plugins (native and WASM) with their status and capabilities.

```bash
voom plugin list
```

#### `voom plugin info`

Show detailed information about a plugin.

```
voom plugin info <NAME>
```

#### `voom plugin enable` / `voom plugin disable`

Enable or disable a plugin.

```
voom plugin enable <NAME>
voom plugin disable <NAME>
```

#### `voom plugin install`

Install a WASM plugin from a `.wasm` file.

```
voom plugin install <PATH>
```

Copies the WASM binary to `~/.config/voom/plugins/wasm/` and registers it.

---

### `voom jobs`

Job management for background processing.

#### `voom jobs list`

List jobs with optional status filter.

```
voom jobs list [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--status <STATUS>` | *none* | Filter by status |
| `-n`, `--limit <N>` | `50` | Maximum number of jobs to display |
| `--offset <N>` | `0` | Number of jobs to skip |

Status values: `pending`, `running`, `completed`, `failed`, `cancelled`.

#### `voom jobs status`

Show details for a specific job.

```
voom jobs status <ID>
```

#### `voom jobs cancel`

Cancel a running or pending job.

```
voom jobs cancel <ID>
```

#### `voom jobs retry`

Retry a failed job.

```
voom jobs retry <ID>
```

#### `voom jobs clear`

Delete completed, failed, or cancelled jobs.

```
voom jobs clear [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--status <STATUS>` | *none* | Only delete jobs with this status |
| `--yes` | `false` | Skip confirmation prompt |

---

### `voom report`

Generate a report of the media library.

```
voom report [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |
| `--library` | `false` | Show full library statistics |
| `--plans` | `false` | Show per-phase plan processing summary |
| `--savings` | `false` | Show space savings breakdown |
| `--period <PERIOD>` | *none* | Time period for savings grouping: `day`, `week`, `month` (requires `--savings`) |
| `--history <N>` | *none* | Show N most recent snapshots |
| `--issues` | `false` | Show files with safeguard violations |
| `--database` | `false` | Show database row counts and page stats |
| `--all` | `false` | Show all report sections |
| `--snapshot` | `false` | Capture and persist a new snapshot |
| `--files` | `false` | List files in the library |
| `--integrity` | `false` | Show aggregate verification and integrity counts |
| `--loudness` | `false` | Show aggregate audio LUFS and true-peak measurements |

`voom report --integrity` reports aggregate counts for total files, never verified files,
stale files using a 30-day cutoff, files with errors, files with warnings, and hash
mismatches. The integrity summary supports `table`, `json`, `plain`, and `csv` formats
through `--format`.

`voom report --loudness` reports measured audio tracks, average integrated LUFS,
average true peak, and files outside the -23 LUFS broadcast target by more than
0.5 LUFS. Use it after running a policy with `normalize`.

---

### `voom bug-report`

Generate and upload sanitized bug reports. Generation and upload are separate
commands so generated files can be reviewed before sharing.

#### `voom bug-report generate`

Generate a local sanitized bug report directory.

```bash
voom bug-report generate --out /tmp/voom-bug [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--out <DIR>` | *required* | Output directory for generated report files |
| `--session <UUID>` | none | Processing session UUID to include |
| `--policy <FILE>` | none | Policy file to include after redaction |
| `--library <PATH>` | none | Library root to include as a redacted path |
| `--event-limit <N>` | `500` | Recent event rows to include |
| `--job-limit <N>` | `100` | Recent jobs to include |

The command writes `report.md`, `report.json`, `redactions.public.json`,
`redactions.local.json`, `metadata.json`, and `README.txt`. Review `report.md`
and `report.json` before sharing. `redactions.local.json` contains original
private values and is never uploaded by VOOM.

#### `voom bug-report upload`

Upload a previously generated sanitized report as a GitHub issue comment
through the `gh` CLI.

```bash
voom bug-report upload /tmp/voom-bug --issue 337 --repo randomparity/voom
```

| Option | Default | Description |
|--------|---------|-------------|
| `<REPORT_DIR>` | *required* | Directory produced by `voom bug-report generate` |
| `--issue <N>` | *required* | GitHub issue number to comment on |
| `--repo <OWNER/NAME>` | `randomparity/voom` | GitHub repository |

`upload` reads `report.md` only. It does not read or upload
`redactions.local.json`.

---

### `voom verify`

Per-file media integrity verification.

#### `voom verify run`

Run verification on files. With no `PATHS`, verifies files that have never been verified
or whose latest verification is older than `--since`. Quick mode is the default.
`--thorough` and `--hash` conflict with each other.

```
voom verify run [PATHS]... [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `[PATHS]...` | due files | File paths to verify |
| `--thorough` | `false` | Run a full ffmpeg decode pass |
| `--hash` | `false` | Run sha256 bit-rot detection against prior hash verification |
| `--since <SINCE>` | `30d` | Re-verify files older than `30d`, `4w`, `12h`, or `YYYY-MM-DD` |
| `--all` | `false` | Re-verify all files regardless of latest verification time |
| `-w`, `--workers <N>` | `0` (auto) | Number of parallel workers for quick and hash modes |
| `-f`, `--format <FORMAT>` | *none* | Accepted; currently unused by `run` output |

**Examples:**

```bash
voom verify run
voom verify run /media/movies/film.mkv
voom verify run --thorough
voom verify run --hash
voom verify run --since 7d
voom verify run --all --workers 8
```

#### `voom verify report`

Show verification records, optionally filtered by file, mode, outcome, or age.

```
voom verify report [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--file <FILE>` | *none* | Show records for one file |
| `--mode <MODE>` | *none* | Filter by mode: `quick`, `thorough`, or `hash` |
| `--outcome <OUTCOME>` | *none* | Filter by outcome: `ok`, `warning`, or `error` |
| `--since <SINCE>` | *none* | Show records since `30d`, `4w`, `12h`, or `YYYY-MM-DD` |
| `--limit <N>` | `100` | Maximum number of records to display |
| `-f`, `--format <FORMAT>` | `table` | Output format; only `json` changes rendering today |

**Examples:**

```bash
voom verify report
voom verify report --outcome error
voom verify report --mode hash --limit 25
voom verify report --file /media/movies/film.mkv
voom verify report --since 30d --format json
```

---

### `voom env`

Environment diagnostics and history.

#### `voom env check`

Run live environment checks. Verifies:
- External tool availability (ffprobe, ffmpeg, mkvpropedit, mkvmerge, mediainfo)
- Tool versions
- Configuration validity
- Database connectivity
- Configured backup destination health
- Plugin status

```bash
voom env check
```

When `[plugin.backup-manager].destinations` is configured, the check reports
one `backup_destination:<name>` result per destination. Rclone-backed
destinations validate rclone availability, remote reachability, and a small
write/delete probe. Output deliberately omits remote URLs and credential-bearing
configuration values.

#### `voom env history`

Show environment check history from the database.

```
voom env history [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--check <NAME>` | *none* | Filter by check name |
| `--since <DATETIME>` | *none* | Show only records since this datetime (e.g. `2024-01-15` or `2024-01-15T10:30:00`) |
| `-n`, `--limit <N>` | `50` | Maximum number of records to display |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

> **Compatibility:** `voom health ...` and `voom doctor` are hidden deprecated
> aliases. Use `voom env ...` for new scripts.

---

### `voom serve`

Start the web server with the dashboard UI.

```
voom serve [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-p`, `--port <PORT>` | `8080` | Port to listen on |
| `--host <ADDR>` | `127.0.0.1` | Host address to bind to |

**Examples:**

```bash
voom serve
voom serve --port 9090 --host 0.0.0.0
```

---

### `voom db`

Database maintenance commands.

#### `voom db prune`

Remove entries for files that no longer exist on disk. Also cleans up associated plans and processing stats for pruned files.

```bash
voom db prune
```

#### `voom db vacuum`

Compact the database to reclaim space.

```bash
voom db vacuum
```

#### `voom db reset`

Reset the database, deleting all data. **This is destructive and cannot be undone.**

Also removes WAL (`voom.db-wal`) and SHM (`voom.db-shm`) companion files to avoid corruption on next open.

```
voom db reset [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--yes` | `false` | Skip confirmation prompt |

#### `voom db list-bad`

List files that failed introspection (corrupt, unreadable, or unparseable media files).

```
voom db list-bad [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--path <PREFIX>` | *none* | Filter by path prefix |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

Shows path, error message, error source, attempt count, file size, and last seen timestamp.

#### `voom db purge-bad`

Remove bad file entries from the database without deleting the files from disk. Use this when you have manually dealt with the files (e.g., fixed or moved them).

```bash
voom db purge-bad
```

#### `voom db clean-bad`

Delete bad files from disk and remove their database entries. Requires confirmation unless `--yes` is set.

```
voom db clean-bad [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--yes` | `false` | Skip confirmation prompt |

Reports how many files were deleted, how many were already missing, and any deletion errors.

---

### `voom config`

Configuration management.

#### `voom config show`

Display the current configuration.

```bash
voom config show
```

#### `voom config edit`

Open the configuration file in your `$EDITOR`.

```bash
voom config edit
```

#### `voom config get`

Get a configuration value by dot-notation key.

```
voom config get <KEY>
```

**Examples:**

```bash
voom config get auth_token
voom config get plugin.ffmpeg-executor.hw_accel
voom config get plugin.ffmpeg-executor.nvenc_max_parallel
```

#### `voom config set`

Set a configuration value by dot-notation key. Auto-detects type (bool, int, float, or string).

```
voom config set <KEY> <VALUE>
```

**Examples:**

```bash
voom config set auth_token mytoken
voom config set plugin.ffmpeg-executor.hw_accel nvenc
voom config set plugin.ffmpeg-executor.nvenc_max_parallel 2
```

Configuration file location: `~/.config/voom/config.toml`

For hardware transcoding, see [Hardware Transcoding](hardware-transcoding.md).

---

### `voom init`

Run first-time setup. Creates the configuration directory, default config file at `~/.config/voom/config.toml`, and a starter policy at `~/.config/voom/policies/default.voom`.

```bash
voom init
```

---

### `voom completions`

Generate shell completions.

```
voom completions <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

**Examples:**

```bash
voom completions bash > ~/.local/share/bash-completion/completions/voom
voom completions zsh > ~/.zfunc/_voom
voom completions fish > ~/.config/fish/completions/voom.fish
```

---

### `voom files`

File queries against the database.

#### `voom files list`

List media files with optional filters.

```
voom files list [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--container <CONTAINER>` | *none* | Filter by container format (e.g. `mkv`, `mp4`) |
| `--codec <CODEC>` | *none* | Filter by codec (e.g. `h265`, `aac`) |
| `--lang <LANG>` | *none* | Filter by language code (e.g. `eng`, `fra`) |
| `--path-prefix <PREFIX>` | *none* | Filter by path prefix |
| `-n`, `--limit <N>` | `100` | Maximum number of files to display |
| `--offset <N>` | `0` | Number of files to skip |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

#### `voom files show`

Show details for a specific file by UUID.

```
voom files show <ID> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<ID>` | *required* | UUID of the media file record |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

#### `voom files delete`

Delete a file record from the database by UUID.

```
voom files delete <ID> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<ID>` | *required* | UUID of the media file record |
| `--yes` | `false` | Skip confirmation prompt |

**Examples:**

```bash
voom files list --container mkv --codec h265
voom files list --lang eng --limit 50
voom files show 550e8400-e29b-41d4-a716-446655440000
voom files delete 550e8400-e29b-41d4-a716-446655440000 --yes
```

---

### `voom plans`

Plan inspection.

#### `voom plans show`

Show plans for a file by UUID or file path.

```
voom plans show <FILE> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<FILE>` | *required* | UUID or file path of the media file |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

**Examples:**

```bash
voom plans show 550e8400-e29b-41d4-a716-446655440000
voom plans show /media/movies/film.mkv --format json
```

---

### `voom events`

View the event log.

```
voom events [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-F`, `--follow` | `false` | Keep streaming new events as they arrive |
| `--filter <PATTERN>` | *none* | Filter by event type pattern (e.g. `file.discovered`, `job.*`) |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |
| `-n`, `--limit <N>` | `50` | Maximum number of events to display |

**Examples:**

```bash
voom events
voom events --follow
voom events --filter "file.*" --limit 100
voom events --filter "job.*" --format json
```

---

### `voom tools`

External tool management.

#### `voom tools list`

List all detected external tools and their status.

```
voom tools list [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

#### `voom tools info`

Show detailed information about a specific tool.

```
voom tools info <NAME> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<NAME>` | *required* | Name of the tool (e.g. `ffmpeg`, `mkvmerge`) |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

**Examples:**

```bash
voom tools list
voom tools list --format json
voom tools info ffmpeg
voom tools info mkvmerge --format json
```

---

### `voom history`

Show the change history (transitions) for a media file.

```
voom history <FILE> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<FILE>` | *required* | Path to the media file |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

**Examples:**

```bash
voom history /media/movies/film.mkv
voom history /media/movies/film.mkv --format json
```

---

### `voom backup`

Backup management.

Remote backup destinations are configured through `[plugin.backup-manager]` in
`~/.config/voom/config.toml`. See [Remote Backup Destinations](remote-backups.md)
for rclone, S3, SFTP, and WebDAV examples.

#### `voom backup list`

List backups (`.vbak` files) in one or more directories.

```
voom backup list [PATH]... [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<PATH>...` | none | Directories to search for local `.vbak` files |
| `--destination <DESTINATION>` | none | List persistent remote backup inventory for one destination |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

#### `voom backup restore`

Restore a file from a `.vbak` backup.

```
voom backup restore <BACKUP_PATH_OR_FILE_PATH> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<BACKUP_PATH_OR_FILE_PATH>` | *required* | Local `.vbak` path, or original file path when `--from` is provided |
| `--from <DESTINATION>` | none | Restore from remote backup inventory for the named destination |
| `--output <PATH>` | none | Write remote restore to this path instead of replacing the original path |
| `--yes` | `false` | Skip confirmation prompt |

#### `voom backup verify`

Verify remote backup inventory against a configured destination.

```
voom backup verify --destination <DESTINATION> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--destination <DESTINATION>` | *required* | Remote backup destination to verify |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

The command reports `verified`, `missing`, `size_mismatch`, `hash_mismatch`, or
`error` for each inventory record and exits non-zero when any record is not
verified.

#### `voom backup cleanup`

Remove local backup files from one or more directories, or remove remote backup
inventory for one destination.

```
voom backup cleanup [PATH]... [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<PATH>...` | none | Directories to remove local `.vbak` backups from |
| `--destination <DESTINATION>` | none | Delete eligible remote backups for one destination |
| `--yes` | `false` | Skip confirmation prompt |

Remote cleanup honors each destination's `minimum_storage_days` setting and
reports skipped records with destination and age.

**Examples:**

```bash
voom backup list /media/movies
voom backup list /media/movies /media/tv --format json
voom backup list --destination offsite
voom backup restore /media/movies/film.mkv.vbak
voom backup restore /media/movies/film.mkv.vbak --yes
voom backup restore /media/movies/film.mkv --from offsite
voom backup restore /media/movies/film.mkv --from offsite --output /tmp/film.mkv
voom backup verify --destination offsite
voom backup verify --destination offsite --format json
voom backup cleanup --destination offsite --yes
voom backup cleanup /media/movies --yes
```

---

## REST API

When running `voom serve`, the following REST API is available:

### Files

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/files` | List files (with filters) |
| GET | `/api/files/:id` | Get file details |
| DELETE | `/api/files/:id` | Delete a file record |
| GET | `/api/files/:id/transitions` | Get file transition history |

### Jobs

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/jobs` | List jobs |
| GET | `/api/jobs/:id` | Get job details |
| GET | `/api/jobs/stats` | Get job statistics |

### Plugins

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/plugins` | List plugins |

### Stats

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/stats` | Get summary statistics |
| GET | `/api/stats/library` | Get full library statistics |
| GET | `/api/stats/history` | Get statistics snapshot history |

### Policy

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/policy/validate` | Validate policy source |
| POST | `/api/policy/format` | Format policy source |

### Tools

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/tools` | List detected external tools |

### Health

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/health` | Get system health status |

### Verification

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/verify` | List verification records; filterable by `mode`, `outcome`, and `limit` |
| GET | `/api/verify/:file_id` | List verification records for one file |
| GET | `/api/integrity-summary` | Aggregate integrity counts |

`/api/integrity-summary` uses the same 30-day stale cutoff as `voom report --integrity`.

### Events

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/events` | SSE stream of live updates |
