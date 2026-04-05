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
| `--on-error <STRATEGY>` | `fail` | Error handling: `continue` or `fail` |
| `-w`, `--workers <N>` | `0` (auto) | Number of parallel workers |
| `--approve` | `false` | Require interactive approval for each file |
| `--no-backup` | `false` | Skip creating backups before modifications |
| `--force-rescan` | `false` | Re-attempt introspection on previously failed files |
| `--flag-size-increase` | `false` | Tag files whose output is larger than the original |
| `--plan-only` | `false` | Output raw plans as JSON to stdout without executing (implies --dry-run) |
| `--priority-by-date` | `false` | Assign job priority based on file modification date |

Before processing, stale database entries for files that no longer exist under the target directory are automatically pruned (along with their associated plans and processing stats). Files that previously failed introspection (tracked as "bad files") are automatically skipped unless `--force-rescan` is set.

Error handling strategies:
- **`fail`** — Stop processing on first error
- **`continue`** — Continue processing remaining phases for the failed file

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
```

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

---

### `voom health`

System health checks and history.

#### `voom health check`

Run live system health checks. Verifies:
- External tool availability (ffprobe, ffmpeg, mkvpropedit, mkvmerge, mediainfo)
- Tool versions
- Configuration validity
- Database connectivity
- Plugin status

```bash
voom health check
```

#### `voom health history`

Show health check history from the database.

```
voom health history [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--check <NAME>` | *none* | Filter by check name |
| `--since <DATETIME>` | *none* | Show only records since this datetime (e.g. `2024-01-15` or `2024-01-15T10:30:00`) |
| `-n`, `--limit <N>` | `50` | Maximum number of records to display |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table`, `json`, `plain`, or `csv` |

> **Note:** `voom doctor` is a hidden alias for `voom health check`.

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
```

Configuration file location: `~/.config/voom/config.toml`

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

## REST API

When running `voom serve`, the following REST API is available:

### Files

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/files` | List files (with filters) |
| GET | `/api/files/:id` | Get file details |
| DELETE | `/api/files/:id` | Delete a file record |

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
| GET | `/api/stats` | Get library statistics |

### Policy

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/policy/validate` | Validate policy source |
| POST | `/api/policy/format` | Format policy source |

### Events

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/events` | SSE stream of live updates |
