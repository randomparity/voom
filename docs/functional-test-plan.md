# VOOM Functional Test Plan

This document walks a QA tester through every user-facing feature of VOOM.
Each section describes the feature, lists prerequisites, and provides
step-by-step validation procedures with expected outcomes.

---

## Prerequisites

### Environment

- Linux or macOS workstation
- Rust toolchain (cargo, rustc)
- External tools installed and on PATH:
  - **Required:** `ffprobe`, `ffmpeg`, `mkvmerge`, `mkvpropedit`
  - **Optional:** `mkvextract`, `mediainfo`, `HandBrakeCLI`
- A directory of sample media files (MKV, MP4, AVI) with varied track layouts:
  - Multiple audio languages
  - Multiple subtitle tracks (including forced/commentary)
  - Font attachments
  - At least one file with HDR or Dolby Vision metadata
  - At least one file not in MKV container

### Build

```bash
cd /path/to/voom
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

Verify the binary runs:

```bash
voom --help
```

**Expected:** Usage text listing all subcommands.

---

## 1. First-Time Setup (`voom init`)

### 1.1 Clean initialization

1. Remove any existing config: `rm -rf ~/.config/voom`
2. Run `voom init`

**Expected:**
- Step-by-step output showing creation of config directory, data directory,
  policies directory, default config file, and database initialization.
- Tool detection results listing found/missing tools with versions.
- "Next steps" guidance printed at the end.

### 1.2 Verify created artifacts

1. `ls ~/.config/voom/config.toml` — file exists
2. `ls ~/.config/voom/voom.db` — database file exists (or is created on first use)
3. `ls ~/.config/voom/policies/` — directory exists

### 1.3 Re-run init (idempotent)

1. Run `voom init` again

**Expected:** Completes without error. Does not overwrite existing config.

---

## 2. Configuration (`voom config`)

### 2.1 Show configuration

1. Run `voom config show`

**Expected:** If a config file exists, displays its contents (with `auth_token`
REDACTED if set). If no config file exists, prints the default configuration
values (serialized from `AppConfig::default()`).

### 2.2 Edit configuration

1. Set `EDITOR=nano` (or any preferred editor)
2. Run `voom config edit`

**Expected:** Editor opens with the config file. After saving, validation
status is printed (valid or error details).

### 2.3 Show with auth token redaction

1. Edit `~/.config/voom/config.toml` and add: `auth_token = "secret123"`
2. Run `voom config show`

**Expected:** The token value appears as REDACTED, not "secret123".

### 2.4 Get a simple config key

1. Run `voom config get scan.workers`

**Expected:** The current value of `scan.workers` is printed (e.g., `4`).

### 2.5 Get a nested config key

1. Run `voom config get db.path`

**Expected:** The current value of `db.path` is printed (e.g., the path to
`voom.db`). Non-existent keys produce an error message.

### 2.6 Set a simple config key

1. Run `voom config set scan.workers 8`
2. Run `voom config get scan.workers`

**Expected:** The set command exits 0. The get command returns `8`.

### 2.7 Set a nested dot-notation key

1. Run `voom config set server.port 9090`
2. Run `voom config get server.port`

**Expected:** The set command exits 0. The get command returns `9090`.
`voom config show` reflects the updated value.

---

## 3. System Health (`voom health`)

### 3.1 Health check

1. Run `voom health check`

**Expected output includes:**
- Config file validity check (OK)
- Database access & schema check (OK)
- Required tools section listing ffprobe, ffmpeg, mkvmerge, mkvpropedit — each
  shows version if found or an error if missing
- Optional tools section listing mkvextract, mediainfo, HandBrakeCLI — warns
  if missing (not an error)
- Plugin count and list of registered plugins
- Summary with total issue count

### 3.2 Missing tool detection

1. Temporarily rename `ffprobe` out of PATH (e.g., `sudo mv /usr/bin/ffprobe /usr/bin/ffprobe.bak`)
2. Run `voom health check`

**Expected:** ffprobe check shows as failed/missing. Restore the tool after testing.

### 3.3 Doctor alias

1. Run `voom doctor`

**Expected:** Identical output to `voom health check`. The `doctor` subcommand
is an alias for `health check`.

### 3.4 Health history (empty)

1. Run `voom health history`

**Expected:** Empty table or message indicating no health history entries found.

### 3.5 Health history after checks

1. Run `voom health check` two or three times
2. Run `voom health history`

**Expected:** Table listing past health check runs with timestamps and summary
status (OK or issue count).

---

## 4. Media Discovery (`voom scan`)

### 4.1 Basic scan

1. Run `voom scan /path/to/media`

**Expected:**
- Discovery progress showing file count as directories are walked
- Hashing progress bar (unless `--no-hash`)
- Introspection progress bar (ffprobe analysis)
- Summary showing total files discovered, any errors

### 4.2 Scan with table output

1. Run `voom scan /path/to/media --format table`

**Expected:** After scan completes, a table of all discovered files is printed
(path, container, size, track count, hash).

### 4.3 Scan without hashing

1. Run `voom scan /path/to/media --no-hash`

**Expected:** Hashing step is skipped entirely. Discovery and introspection
still occur. Files stored without content hash.

### 4.4 Scan non-recursive

1. Place media files in both `/path/to/media/` and `/path/to/media/subdir/`
2. Run `voom scan /path/to/media --recursive=false`

**Expected:** Only files in the top-level directory are discovered; subdirectory
files are not included.

### 4.5 Scan with worker count

1. Run `voom scan /path/to/media --workers 1`

**Expected:** Scan completes (single-threaded). Compare timing with default
(auto) worker count.

### 4.6 Scan non-existent path

1. Run `voom scan /nonexistent/path`

**Expected:** Error message indicating the path does not exist. Non-zero exit code.

### 4.7 Scan directory with no media files

1. Create an empty directory or one with only non-media files (e.g., `.txt`)
2. Run `voom scan /path/to/empty`

**Expected:** Scan completes with 0 files discovered.

### 4.8 Scan multiple directories

1. Run `voom scan /path/to/media1 /path/to/media2`

**Expected:** Both directories are walked. Discovery progress reflects files
from both paths. Summary shows combined file count from all specified directories.

---

## 5. File Inspection (`voom inspect`)

### 5.1 Table output (default)

1. Run `voom inspect /path/to/media/sample.mkv`

**Expected:** Two sections:
- **File info:** path, container format, file size, duration, overall bitrate,
  content hash, internal ID
- **Track table:** columns for track type, codec, language, default flag,
  forced flag, and type-specific details (resolution/fps/HDR for video;
  channels/sample rate for audio)

### 5.2 JSON output

1. Run `voom inspect /path/to/media/sample.mkv --format json`

**Expected:** Valid JSON object containing file metadata and tracks array.
Verify with `| jq .` that it parses cleanly.

### 5.3 Tracks only

1. Run `voom inspect /path/to/media/sample.mkv --tracks-only`

**Expected:** Only the track table is shown; file-level metadata is omitted.

### 5.4 Inspect non-existent file

1. Run `voom inspect /nonexistent/file.mkv`

**Expected:** Error message. Non-zero exit code.

---

## 6. Policy Management (`voom policy`)

### 6.1 Validate a correct policy

1. Create a minimal policy file `test.voom`:
   ```
   policy "test" {
     config {
       languages audio: [eng]
       on_error: continue
     }
     phase clean {
       keep audio where lang in [eng]
     }
   }
   ```
2. Run `voom policy validate test.voom`

**Expected:** Validation passes. Shows phase count (1) and phase order.

### 6.2 Validate a policy with errors

1. Create `bad.voom` with a syntax error (e.g., missing closing brace)
2. Run `voom policy validate bad.voom`

**Expected:** Parse or validation error with line/column information.

### 6.3 Validate semantic errors

1. Create a policy with circular dependencies:
   ```
   policy "circular" {
     config { on_error: abort }
     phase a { depends_on: [b] }
     phase b { depends_on: [a] }
   }
   ```
2. Run `voom policy validate circular.voom`

**Expected:** Validation error reporting circular dependency.

### 6.4 Show compiled policy

1. Run `voom policy show test.voom` (using the valid policy from 6.1)

**Expected:** Displays policy name, config section (languages, on_error),
phases with their dependencies and operations, and full JSON representation
of the compiled policy.

### 6.5 Format a policy

1. Create a policy file with inconsistent indentation and spacing
2. Run `voom policy format messy.voom`

**Expected:** File is reformatted in-place with consistent indentation.
Re-parse produces identical AST (round-trip safe).

### 6.6 List policies

1. Copy one or more `.voom` files into `~/.config/voom/policies/`
2. Run `voom policy list`

**Expected:** Table listing each policy file with name, phase count, and
validation status (OK or ERR).

### 6.7 List with no policies

1. Ensure `~/.config/voom/policies/` is empty
2. Run `voom policy list`

**Expected:** Message indicating no policies found, or an empty table.

### 6.8 Policy diff — two different policies

1. Create `policy_a.voom` and `policy_b.voom` with different phase contents
2. Run `voom policy diff policy_a.voom policy_b.voom`

**Expected:** Diff output highlighting differences between the two compiled
policies (added/removed phases, changed operations, config differences).

### 6.9 Policy diff — identical policies

1. Run `voom policy diff test.voom test.voom`

**Expected:** Output indicates policies are identical. Exit code 0.

---

## 7. Processing (`voom process`)

### 7.1 Dry run

1. Scan a media directory first: `voom scan /path/to/media`
2. Run `voom process /path/to/media --policy test.voom --dry-run`

**Expected:** For each file, the plan of operations is displayed (what would
be kept, removed, reordered, transcoded) without executing any changes. No
files are modified.

### 7.2 Process with backup (default)

1. Run `voom process /path/to/media/single-file.mkv --policy test.voom`

**Expected:**
- Backup of the original file is created before modifications
- Processing progress with worker count displayed
- Summary shows processed/skipped/error counts
- Verify the original file was backed up (check for backup file or directory)

### 7.3 Process without backup

1. Run `voom process /path/to/media/file.mkv --policy test.voom --no-backup`

**Expected:** Processing occurs without creating a backup. File is modified
in-place.

### 7.4 Error strategy: continue

1. Use a policy that will fail on some files
2. Run `voom process /path/to/media --policy test.voom --on-error continue`

**Expected:** Errors are logged but processing continues for all files.
Summary shows error count alongside processed count.

### 7.5 Error strategy: fail

1. Run `voom process /path/to/media --policy test.voom --on-error fail`

**Expected:** Processing stops at the first error. Remaining files are not
processed.

### 7.6 Worker count control

1. Run `voom process /path/to/media --policy test.voom --dry-run --workers 2`

**Expected:** Output indicates 2 workers are used.

### 7.7 Missing policy file

1. Run `voom process /path/to/media --policy nonexistent.voom`

**Expected:** Error message about missing policy file. Non-zero exit code.

### 7.8 Multi-path processing

1. Run `voom process /path/to/media1 /path/to/media2 --policy test.voom --dry-run`

**Expected:** Files from both directories appear in the plan output. Summary
reflects combined file count from all specified paths.

### 7.9 Plan only

1. Run `voom process /path/to/media --policy test.voom --plan-only`

**Expected:** Plans are generated and displayed (or saved) for all files but
no executor is invoked. No files are modified. Output is similar to dry-run
but emphasizes the plan structure rather than a human-readable summary.

### 7.10 Policy map

1. Create two policy files `policy_a.voom` and `policy_b.voom`
2. Run `voom process /path/to/media --policy-map /path/to/media/subdir:policy_a.voom --policy-map /path/to/media:policy_b.voom --dry-run`

**Expected:** Files under `/path/to/media/subdir` use `policy_a.voom`;
remaining files under `/path/to/media` use `policy_b.voom`. The dry-run
output labels each file with the policy applied.

---

## 8. Job Management (`voom jobs`)

### 8.1 List jobs (empty)

1. Run `voom jobs list`

**Expected:** Empty table or message indicating no jobs found.

### 8.2 List jobs after processing

1. Run a `voom process` command (from section 7)
2. Run `voom jobs list`

**Expected:** Table with columns: ID (UUID), type, status (color-coded),
progress %, worker ID, creation time. Summary counts by status at the bottom.

### 8.3 Filter jobs by status

1. Run `voom jobs list --status completed`

**Expected:** Only completed jobs are shown.

2. Run `voom jobs list --status failed`

**Expected:** Only failed jobs are shown (or empty if none failed).

### 8.4 Job status detail

1. Note a job ID from `voom jobs list`
2. Run `voom jobs status <job-id>`

**Expected:** Detailed job info: ID, type, status, progress %, message,
error (if any), timestamps (created, started, completed).

### 8.5 Cancel a job

1. Start a long-running process in background or note a pending job ID
2. Run `voom jobs cancel <job-id>`

**Expected:** Confirmation message that job was cancelled.

### 8.6 Invalid job ID

1. Run `voom jobs status 00000000-0000-0000-0000-000000000000`

**Expected:** Error message indicating job not found.

---

## 9. Reports (`voom report`)

### 9.1 Library report

1. Scan media files first (section 4)
2. Run `voom report --library`

**Expected:** Library summary showing total files, total size, total duration,
container breakdown (count per format), and codec breakdown (count per codec).

### 9.2 Plans report

1. Run a dry-run or process to generate plans
2. Run `voom report --plans`

**Expected:** Summary of recent plans — files evaluated, operations proposed
(keep, remove, transcode counts), and any warnings from the evaluator.

### 9.3 Savings report

1. Run `voom report --savings`

**Expected:** Estimated or realized storage savings — size before and after
processing, delta in bytes and percentage.

### 9.4 Period filter

1. Run `voom report --library --period 7d`

**Expected:** Library statistics scoped to the last 7 days (files scanned or
processed within that window).

2. Run `voom report --library --period 30d`

**Expected:** Statistics scoped to the last 30 days.

### 9.5 History report

1. Run `voom report --history`

**Expected:** Chronological log of past scan and process operations with
timestamps, file counts, and outcomes.

### 9.6 Issues report

1. Run `voom report --issues`

**Expected:** List of files that had warnings or errors during the most recent
scan or process run. Empty list if no issues.

### 9.7 Database report

1. Run `voom report --database`

**Expected:** Database statistics — file count, record count, database file
size, last vacuum timestamp.

### 9.8 All report

1. Run `voom report --all`

**Expected:** Combines all report sections into one output. Equivalent to
running `--library --plans --savings --history --issues --database` together.

### 9.9 Snapshot

1. Run `voom report --snapshot`

**Expected:** A point-in-time snapshot of current library state is saved to
disk (or printed). Subsequent runs can be compared against it.

### 9.10 Files listing in report

1. Run `voom report --files`

**Expected:** Tabular listing of all files in the library with key metadata
columns (path, container, size, last processed).

### 9.11 JSON output

1. Run `voom report --library --format json`

**Expected:** Valid JSON output. Verify with `| jq .` that it parses cleanly.
Keys include at minimum `total_files`, `total_size`, `containers`, `codecs`.

### 9.12 Report with empty library

1. Reset the database: `voom db reset` (confirm with "yes")
2. Run `voom report --library`

**Expected:** Zeros for all statistics or an appropriate empty-state message.

---

## 10. Database Management (`voom db`)

### 10.1 Prune stale entries

1. Scan a directory, then delete or move some of the scanned media files
2. Run `voom db prune`

**Expected:** Reports count of deleted stale entries (files no longer on disk).

### 10.2 Prune with no stale entries

1. Run `voom db prune` when all scanned files still exist

**Expected:** "No stale entries found" or similar message.

### 10.3 Vacuum database

1. Run `voom db vacuum`

**Expected:** Confirmation message. Database file size may decrease after
pruning + vacuum.

### 10.4 Reset database

1. Run `voom db reset`

**Expected:** Interactive confirmation prompt requiring the user to type "yes".
After confirmation, the database file is deleted and a new empty one is created.

### 10.5 Reset database — decline

1. Run `voom db reset`, type "no" at the prompt

**Expected:** Operation is cancelled. Database is unchanged.

---

## 11. Plugin Management (`voom plugin`)

### 11.1 List plugins

1. Run `voom plugin list`

**Expected:** Table with columns: name, version, capabilities. All native
plugins listed (sqlite-store, tool-detector, discovery, ffprobe-introspector,
policy-evaluator, phase-orchestrator, mkvtoolnix-executor, ffmpeg-executor,
backup-manager, job-manager, web-server). Disabled plugins shown separately.

### 11.2 Plugin info

1. Run `voom plugin info discovery`

**Expected:** Plugin name, version, status (enabled/disabled), and list of
capabilities.

### 11.3 Disable a plugin

1. Run `voom plugin disable web-server`
2. Run `voom plugin list`

**Expected:** web-server appears in the disabled section. Config file is
updated with `disabled_plugins = ["web-server"]`.

### 11.4 Enable a disabled plugin

1. Run `voom plugin enable web-server`
2. Run `voom plugin list`

**Expected:** web-server appears in the enabled section again.

### 11.5 Info for unknown plugin

1. Run `voom plugin info nonexistent-plugin`

**Expected:** Error message indicating plugin not found.

### 11.6 Install WASM plugin

1. Obtain or build a `.wasm` plugin file with a sibling `.toml` manifest
2. Run `voom plugin install /path/to/plugin.wasm`

**Expected:** Plugin and manifest are copied to `~/.config/voom/plugins/wasm/`.
Success message displayed.

### 11.7 Install WASM plugin — missing manifest

1. Create a dummy `.wasm` file without a `.toml` manifest
2. Run `voom plugin install /path/to/dummy.wasm`

**Expected:** Error about missing manifest file.

---

## 12. Web Server (`voom serve`)

### 12.1 Start server (default)

1. Run `voom serve`

**Expected:** Server starts on `http://127.0.0.1:8080`. Startup message
printed with URL.

2. Open `http://127.0.0.1:8080` in a browser.

**Expected:** Dark-themed dashboard page loads with library stats and job
status.

3. Stop the server with Ctrl+C.

### 12.2 Custom host and port

1. Run `voom serve --host 0.0.0.0 --port 9090`

**Expected:** Server starts on `http://0.0.0.0:9090`.

### 12.3 Web pages

Navigate to each page and verify it loads without errors:

| URL | Expected Content |
|-----|-----------------|
| `/` | Dashboard with file counts and job stats |
| `/library` | File browser table with pagination controls |
| `/library?container=mkv` | Filtered file list showing only MKV files |
| `/files/<id>` | File detail page (use a valid ID from the library page) |
| `/policies` | Policy management listing |
| `/policies/<name>/edit` | Policy editor with syntax input |
| `/jobs` | Job monitoring table |
| `/plugins` | Plugin management listing |
| `/settings` | Application settings page |

### 12.4 Security headers

1. Run `curl -I http://127.0.0.1:8080/`

**Expected headers present:**
- `X-Frame-Options: DENY`
- `X-Content-Type-Options: nosniff`
- `Referrer-Policy: strict-origin-when-cross-origin`
- `Content-Security-Policy` header with restrictive policy

---

## 13. REST API

Start the server (`voom serve`) before running these tests. Use `curl` or
any HTTP client.

### 13.1 Files API

**List files:**
```bash
curl http://127.0.0.1:8080/api/files
```
**Expected:** JSON `{"files": [...], "total": N}`

**List with filters:**
```bash
curl "http://127.0.0.1:8080/api/files?container=mkv&limit=5"
```
**Expected:** Only MKV files, at most 5 results.

**Get single file:**
```bash
curl http://127.0.0.1:8080/api/files/<uuid>
```
**Expected:** JSON object with file metadata and tracks.

**Delete file record:**
```bash
curl -X DELETE http://127.0.0.1:8080/api/files/<uuid>
```
**Expected:** 200 OK. File record removed from database (actual file on disk
is not deleted).

**Get non-existent file:**
```bash
curl http://127.0.0.1:8080/api/files/00000000-0000-0000-0000-000000000000
```
**Expected:** 404 response.

**File transitions:**
```bash
curl http://127.0.0.1:8080/api/files/<uuid>/transitions
```
**Expected:** JSON listing state transitions for the file (scan, process,
backup events) with timestamps.

### 13.2 Jobs API

**List jobs:**
```bash
curl http://127.0.0.1:8080/api/jobs
```
**Expected:** JSON `{"jobs": [...]}`

**Filter by status:**
```bash
curl "http://127.0.0.1:8080/api/jobs?status=completed"
```
**Expected:** Only completed jobs.

**Job details:**
```bash
curl http://127.0.0.1:8080/api/jobs/<uuid>
```
**Expected:** Full job object with progress, timestamps, error details.

**Job stats:**
```bash
curl http://127.0.0.1:8080/api/jobs/stats
```
**Expected:** JSON `{"counts": [{"status": "completed", "count": N}, ...]}`

### 13.3 Plugins API

```bash
curl http://127.0.0.1:8080/api/plugins
```
**Expected:** JSON `{"plugins": [{"name": "...", "version": "...", "capabilities": [...]}]}`

### 13.4 Statistics API

```bash
curl http://127.0.0.1:8080/api/stats
```
**Expected:** JSON `{"total_files": N, "total_jobs": [{"status": "...", "count": N}]}`

### 13.5 Tools API

```bash
curl http://127.0.0.1:8080/api/tools
```
**Expected:** JSON listing detected external tools with their versions.

### 13.6 Health API

```bash
curl http://127.0.0.1:8080/api/health
```
**Expected:** JSON with current health status — database connectivity, tool
availability, plugin count, and overall status field (`ok` or `degraded`).

### 13.7 Policy validation API

**Valid policy:**
```bash
curl -X POST http://127.0.0.1:8080/api/policy/validate \
  -H "Content-Type: application/json" \
  -d '{"source": "policy \"test\" { config { on_error: abort } phase p { container mkv } }"}'
```
**Expected:** `{"valid": true, "errors": []}`

**Invalid policy:**
```bash
curl -X POST http://127.0.0.1:8080/api/policy/validate \
  -H "Content-Type: application/json" \
  -d '{"source": "policy { broken"}'
```
**Expected:** `{"valid": false, "errors": [{"message": "..."}]}`

### 13.8 Policy format API

```bash
curl -X POST http://127.0.0.1:8080/api/policy/format \
  -H "Content-Type: application/json" \
  -d '{"source": "policy \"test\" { config { on_error: abort } phase p { container mkv } }"}'
```
**Expected:** `{"formatted": "..."}` with consistently indented output.

### 13.9 Authentication

1. Set `auth_token = "testtoken"` in config.toml
2. Restart `voom serve`
3. `curl http://127.0.0.1:8080/api/files`

**Expected:** 401 Unauthorized.

4. `curl -H "Authorization: Bearer testtoken" http://127.0.0.1:8080/api/files`

**Expected:** 200 OK with file listing.

5. Verify HTML pages (e.g., `curl http://127.0.0.1:8080/`) are still accessible
   without auth.

### 13.10 Server-Sent Events

```bash
curl -N http://127.0.0.1:8080/events
```
**Expected:** Connection stays open. When a scan or process runs concurrently,
events appear as `data: {...}\n\n` lines. If auth is enabled, this endpoint
also requires the Bearer token.

---

## 14. DSL Features (comprehensive policy validation)

These tests validate that the DSL parser and compiler handle all language
constructs correctly. Use `voom policy validate` and `voom policy show` for each.

### 14.1 Minimal policy

```
policy "minimal" {
  config { on_error: abort }
  phase p { container mkv }
}
```
**Expected:** Validates OK. Show output displays 1 phase.

### 14.2 Track filtering with all operators

```
policy "filters" {
  config { on_error: abort }
  phase p {
    keep audio where lang in [eng, fra, deu]
    keep audio where lang == eng
    keep audio where codec in [aac, opus]
    keep audio where codec == aac
    keep audio where channels >= 6
    keep audio where not commentary
    keep subtitles where forced
    keep subtitles where default
    keep subtitles where title contains "English"
    remove attachments where not font
    keep audio where lang == eng and not commentary or forced
  }
}
```
**Expected:** Validates OK. All filter types are accepted.

### 14.3 Phase dependencies and conditional execution

```
policy "phases" {
  config { on_error: continue }
  phase a { container mkv }
  phase b {
    depends_on: [a]
    skip when video.codec in [hevc]
    keep audio where lang == eng
  }
  phase c {
    depends_on: [a, b]
    run_if b.modified
    container mp4
  }
}
```
**Expected:** Validates OK. `voom policy show` displays dependency graph
and phase order (a, b, c via topological sort).

### 14.4 Transcoding with all options

```
policy "transcode" {
  config { on_error: abort }
  phase p {
    transcode video to hevc {
      crf: 20
      preset: medium
      max_resolution: 1080p
      scale_algorithm: lanczos
      hw: auto
      hw_fallback: true
    }
    transcode audio to aac {
      preserve: [truehd, dts_hd, flac]
      bitrate: 192k
      channels: stereo
    }
  }
}
```
**Expected:** Validates OK. Compiled policy includes transcode operations
with all parameters.

### 14.5 Synthesize block

```
policy "synth" {
  config { on_error: abort }
  phase p {
    synthesize "Stereo AAC" {
      codec: aac
      channels: stereo
      source: prefer(codec in [truehd, flac] and channels >= 6)
      bitrate: "192k"
      skip_if_exists { codec in [aac] and channels == 2 }
      title: "Stereo (AAC)"
      language: inherit
      position: after_source
    }
  }
}
```
**Expected:** Validates OK.

### 14.6 Conditional blocks and rules

```
policy "conditionals" {
  config { on_error: abort }
  phase p {
    when exists(audio where lang == jpn) and not exists(subtitle where lang == eng) {
      warn "Japanese audio but no English subtitles"
    }
    else {
      set_default audio where lang == eng and default
    }
    when count(audio) >= 3 {
      warn "Too many audio tracks"
    }
    when audio_is_multi_language {
      warn "Multiple languages detected"
    }
    rules first {
      rule "check" {
        when exists(audio where commentary) {
          warn "Has commentary"
        }
      }
    }
  }
}
```
**Expected:** Validates OK.

### 14.7 Plugin metadata field access

```
policy "metadata" {
  config { on_error: abort }
  phase p {
    when plugin.radarr.title exists {
      set_tag "title" plugin.radarr.title
      set_language audio where default plugin.radarr.original_language
    }
  }
}
```
**Expected:** Validates OK.

### 14.8 Full production policy

1. Run `voom policy validate crates/voom-dsl/tests/fixtures/production-normalize.voom`

**Expected:** Validates OK. 6 phases in correct dependency order.

### 14.9 Codec normalization

1. Create a policy using `h265` (alias for `hevc`)
2. Run `voom policy show` on it

**Expected:** Compiled output normalizes `h265` to `hevc`.

### 14.10 Unknown codec detection

```
policy "bad-codec" {
  config { on_error: abort }
  phase p {
    keep audio where codec == xyzabc
  }
}
```
**Expected:** Validation warning about unknown codec, possibly with a
did-you-mean suggestion.

### 14.11 Container metadata operations

```
policy "tags" {
  config { on_error: abort }
  phase p {
    clear_tags
    set_tag "title" "My Movie"
    delete_tag "encoder"
  }
}
```
**Expected:** Validates OK.

### 14.12 Duplicate phase names

```
policy "dup" {
  config { on_error: abort }
  phase p { container mkv }
  phase p { container mp4 }
}
```
**Expected:** Validation error reporting duplicate phase name.

---

## 15. Shell Completions (`voom completions`)

### 15.1 Bash completions

```bash
voom completions bash > /dev/null
```
**Expected:** Exits 0. Output is valid bash completion script.

### 15.2 Zsh completions

```bash
voom completions zsh > /dev/null
```
**Expected:** Exits 0.

### 15.3 Fish completions

```bash
voom completions fish > /dev/null
```
**Expected:** Exits 0.

---

## 16. File Listing (`voom files`)

### 16.1 List all files

1. Scan a media directory first (section 4)
2. Run `voom files list`

**Expected:** Table of all files in the library with columns for path,
container, size, and last-scanned timestamp.

### 16.2 List with filters

1. Run `voom files list --container mkv`

**Expected:** Only MKV files are shown.

2. Run `voom files list --codec hevc`

**Expected:** Only files containing an HEVC video track are shown.

### 16.3 Pagination

1. Run `voom files list --limit 10 --offset 0`

**Expected:** First 10 files returned.

2. Run `voom files list --limit 10 --offset 10`

**Expected:** Next 10 files returned (second page).

### 16.4 Show a single file

1. Note a file UUID from `voom files list`
2. Run `voom files show <uuid>`

**Expected:** Detailed output matching `voom inspect` for that file — metadata
and full track listing.

### 16.5 Delete a file record

1. Run `voom files delete <uuid>`

**Expected:** Confirmation prompt or immediate success message. File record
is removed from the database. The actual file on disk is not deleted.

---

## 17. Plan Viewer (`voom plans`)

### 17.1 Show plan by UUID

1. Run a dry-run to generate plans: `voom process /path/to/media --policy test.voom --dry-run`
2. Note a plan UUID from the output
3. Run `voom plans show <uuid>`

**Expected:** Full plan detail — file path, policy applied, phase breakdown,
each operation (keep/remove/transcode) with track details, and estimated outcome.

### 17.2 Show plan by file path

1. Run `voom plans show /path/to/media/sample.mkv`

**Expected:** Most recent plan for that file path is displayed with the same
detail as 17.1.

### 17.3 Plan in JSON format

1. Run `voom plans show <uuid> --format json`

**Expected:** Valid JSON representation of the plan. Verify with `| jq .`

---

## 18. Event Log (`voom events`)

### 18.1 View recent events

1. Run `voom events`

**Expected:** Table of recent system events — scan started/completed,
files discovered, process events, errors — with timestamps and event types.

### 18.2 Follow events (live tail)

1. Run `voom events --follow` in one terminal
2. In another terminal, run `voom scan /path/to/media`

**Expected:** New events appear in real time in the first terminal as the
scan progresses. Stop with Ctrl+C.

### 18.3 Filter events by type

1. Run `voom events --type scan`

**Expected:** Only scan-related events are shown.

2. Run `voom events --type error`

**Expected:** Only error events are shown (or empty if none).

---

## 19. Tool Information (`voom tools`)

### 19.1 List tools

1. Run `voom tools list`

**Expected:** Table of detected external tools with name, path, and version.
Required tools (ffprobe, ffmpeg, mkvmerge, mkvpropedit) and optional tools
(mkvextract, mediainfo, HandBrakeCLI) are listed separately.

### 19.2 Tool info

1. Run `voom tools info ffprobe`

**Expected:** Detailed info for ffprobe — full path, version string, detected
capabilities.

### 19.3 Tools in JSON format

1. Run `voom tools list --format json`

**Expected:** Valid JSON listing all tools. Verify with `| jq .`

---

## 20. Processing History (`voom history`)

### 20.1 View history

1. Run a process command (section 7) to generate history
2. Run `voom history`

**Expected:** Chronological table of past process runs — timestamp, path
processed, policy used, file count, outcome (completed/failed/partial).

### 20.2 History in JSON format

1. Run `voom history --format json`

**Expected:** Valid JSON array of history entries. Verify with `| jq .`

---

## 21. Backup Management (`voom backup`)

### 21.1 List backups

1. Process a file with backup enabled (section 7.2)
2. Run `voom backup list`

**Expected:** Table of backup entries — original file path, backup path,
size, and timestamp.

### 21.2 List backups for multiple directories

1. Process files from two different directories with backup enabled
2. Run `voom backup list /path/to/media1 /path/to/media2`

**Expected:** Backup entries for files from both directories are shown.

### 21.3 Restore a backup

1. Note a backup entry from `voom backup list`
2. Run `voom backup restore <backup-id>`

**Expected:** Confirmation prompt. After confirmation, the backup is restored
to the original path, replacing the current file.

### 21.4 Cleanup old backups

1. Run `voom backup cleanup`

**Expected:** Interactive confirmation prompt listing backups that would be
removed (based on age or policy). Declines without modification on "no".

### 21.5 Cleanup with auto-confirm

1. Run `voom backup cleanup --yes`

**Expected:** Backup cleanup proceeds without prompting. Summary shows how
many backups were removed and space reclaimed.

---

## 22. Verbose Output

1. Run any command with `-v` flag: `voom -v scan /path/to/media`

**Expected:** Additional info-level log messages appear.

2. Run with `-vv`: `voom -vv health check`

**Expected:** Debug-level log messages appear.

3. Run with `-vvv`: `voom -vvv health check`

**Expected:** Trace-level log messages appear.

---

## 23. Error Handling & Edge Cases

### 23.1 No arguments

1. Run `voom` with no subcommand

**Expected:** Help text displayed. Zero or non-zero exit code depending on
clap configuration.

### 23.2 Unknown subcommand

1. Run `voom foobar`

**Expected:** Error message with suggestion of similar commands. Non-zero
exit code.

### 23.3 Permission denied

1. Run `voom scan /root` (or another directory without read access)

**Expected:** Appropriate error message. Scan reports the inaccessible paths.

### 23.4 Insufficient disk space for backup

1. Process a file on a nearly-full filesystem with backup enabled

**Expected:** Error message about insufficient disk space. File is not
modified (backup-manager validates available space before proceeding).

### 23.5 Large policy input to API

1. Send a POST to `/api/policy/validate` with a body exceeding 1 MiB

**Expected:** Rejected with appropriate error (request too large).

---

## 24. End-to-End Workflow

This test validates a complete real-world workflow from start to finish.

1. **Initialize:** `voom init`
2. **Check health:** `voom health check` — all required tools OK
3. **Scan library:** `voom scan /path/to/media --format table`
   - Verify file count matches expected number of media files
4. **Inspect a file:** `voom inspect /path/to/media/sample.mkv`
   - Verify tracks match what ffprobe reports
5. **Check library report:** `voom report --library` — file count matches scan
6. **Create a policy:** Write a `.voom` file with normalize + transcode phases
7. **Validate policy:** `voom policy validate policy.voom` — OK
8. **Dry run:** `voom process /path/to/media --policy policy.voom --dry-run`
   - Review plans for each file; verify they are sensible
9. **Process one file:** `voom process /path/to/media/sample.mkv --policy policy.voom`
   - Verify backup was created
   - Verify output file has expected tracks/container
10. **Check jobs:** `voom jobs list` — shows completed job for the file
11. **Generate report:** `voom report --library` — stats reflect the processed file
12. **Start web server:** `voom serve` (in background)
13. **Verify API:** `curl http://127.0.0.1:8080/api/files` — returns the processed file
14. **Verify dashboard:** Open `http://127.0.0.1:8080/` — shows updated stats
15. **Cleanup:** `voom db prune && voom db vacuum`
