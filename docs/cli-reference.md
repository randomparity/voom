# VOOM CLI Reference

## Global Options

```
voom [OPTIONS] <COMMAND>
```

| Option | Description |
|--------|-------------|
| `-v`, `--verbose` | Increase verbosity. `-v` = info, `-vv` = debug, `-vvv` = trace |
| `--version` | Print version |
| `--help` | Print help |

Verbosity can also be controlled via the `RUST_LOG` environment variable (e.g., `RUST_LOG=debug`).

---

## Commands

### `voom scan`

Discover and introspect media files in a directory.

```
voom scan <PATH> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<PATH>` | *required* | Directory to scan for media files |
| `-r`, `--recursive` | `true` | Recurse into subdirectories |
| `-w`, `--workers <N>` | `0` (auto) | Number of parallel workers for hashing |
| `--no-hash` | `false` | Skip content hashing (faster scans) |

Before scanning, stale database entries for files that no longer exist under the scanned directory are automatically pruned (along with their associated plans and processing stats).

The scanner walks the directory tree (using rayon for parallelism), identifies media files by extension, computes xxHash64 content hashes (unless `--no-hash`), and runs ffprobe for metadata extraction. Results are stored in the SQLite database. Files that fail introspection are recorded as "bad files" for tracking and can be reviewed with `voom db list-bad`.

**Examples:**

```bash
voom scan /media/movies -r
voom scan /media/tv --workers 8 --no-hash
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
| `-f`, `--format <FORMAT>` | `table` | Output format: `table` or `json` |
| `--tracks-only` | `false` | Show only track information |

**Examples:**

```bash
voom inspect movie.mkv
voom inspect movie.mkv --format json
voom inspect movie.mkv --tracks-only
```

---

### `voom process`

Apply a policy to media files.

```
voom process <PATH> --policy <FILE> [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<PATH>` | *required* | Directory or file to process |
| `-p`, `--policy <FILE>` | *required* | Policy file (`.voom`) to apply |
| `--dry-run` | `false` | Show what would be done without making changes |
| `--on-error <STRATEGY>` | `fail` | Error handling: `skip`, `continue`, or `fail` |
| `-w`, `--workers <N>` | `0` (auto) | Number of parallel workers |
| `--approve` | `false` | Require interactive approval for each file |
| `--no-backup` | `false` | Skip creating backups before modifications |
| `--force-rescan` | `false` | Re-attempt introspection on previously failed files |

Before processing, stale database entries for files that no longer exist under the target directory are automatically pruned (along with their associated plans and processing stats). Files that previously failed introspection (tracked as "bad files") are automatically skipped unless `--force-rescan` is set.

Error handling strategies:
- **`fail`** — Stop processing on first error
- **`skip`** — Skip the failed file and continue with others
- **`continue`** — Continue processing remaining phases for the failed file

**Examples:**

```bash
# Dry run to preview changes
voom process /media/movies --policy normalize.voom --dry-run

# Process with 4 workers, skipping errors
voom process /media/movies --policy normalize.voom --workers 4 --on-error skip

# Process with interactive approval
voom process movie.mkv --policy normalize.voom --approve
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

**Examples:**

```bash
voom policy validate my-policy.voom
voom policy format my-policy.voom
voom policy show my-policy.voom
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
voom jobs list [--status <STATUS>]
```

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

---

### `voom report`

Generate a report of the media library.

```
voom report [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-f`, `--format <FORMAT>` | `table` | Output format: `table` or `json` |

---

### `voom doctor`

Run a system health check. Verifies:
- External tool availability (ffprobe, ffmpeg, mkvpropedit, mkvmerge, mediainfo)
- Tool versions
- Configuration validity
- Database connectivity
- Plugin status

```bash
voom doctor
```

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

```bash
voom db reset
```

#### `voom db list-bad`

List files that failed introspection (corrupt, unreadable, or unparseable media files).

```
voom db list-bad [OPTIONS]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--path <PREFIX>` | *none* | Filter by path prefix |
| `-f`, `--format <FORMAT>` | `table` | Output format: `table` or `json` |

Shows path, error message, error source, attempt count, and last seen timestamp.

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

Configuration file location: `~/.config/voom/config.toml`

---

### `voom init`

Run first-time setup. Creates the configuration directory and default config file at `~/.config/voom/config.toml`.

```bash
voom init
```

---

### `voom status`

Show library and daemon status — file counts, container breakdown, bad file count, plugin count, and configuration paths.

If bad files exist, the count is highlighted in red with a hint to run `voom db list-bad`.

```bash
voom status
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
