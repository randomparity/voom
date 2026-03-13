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

**Expected:** Displays the contents of `config.toml` with all documented
options. If an `auth_token` is set, its value is REDACTED in output.

### 2.2 Edit configuration

1. Set `EDITOR=nano` (or any preferred editor)
2. Run `voom config edit`

**Expected:** Editor opens with the config file. After saving, validation
status is printed (valid or error details).

### 2.3 Show with auth token redaction

1. Edit `~/.config/voom/config.toml` and add: `auth_token = "secret123"`
2. Run `voom config show`

**Expected:** The token value appears as REDACTED, not "secret123".

---

## 3. System Health (`voom doctor`)

1. Run `voom doctor`

**Expected output includes:**
- Config file validity check (OK)
- Database access & schema check (OK)
- Required tools section listing ffprobe, ffmpeg, mkvmerge, mkvpropedit — each
  shows version if found or an error if missing
- Optional tools section listing mkvextract, mediainfo, HandBrakeCLI — warns
  if missing (not an error)
- Plugin count and list of registered plugins
- Summary with total issue count

### 3.1 Missing tool detection

1. Temporarily rename `ffprobe` out of PATH (e.g., `sudo mv /usr/bin/ffprobe /usr/bin/ffprobe.bak`)
2. Run `voom doctor`

**Expected:** ffprobe check shows as failed/missing. Restore the tool after testing.

---

## 4. Library Status (`voom status`)

1. Run `voom status` (before scanning any files)

**Expected:** Shows library stats (0 files, 0 bytes), plugin count, config
and data directory paths.

2. Scan some files (see section 5), then run `voom status` again.

**Expected:** Updated file count, total size, and top 5 container formats.

---

## 5. Media Discovery (`voom scan`)

### 5.1 Basic scan

1. Run `voom scan /path/to/media`

**Expected:**
- Discovery progress showing file count as directories are walked
- Hashing progress bar (unless `--no-hash`)
- Introspection progress bar (ffprobe analysis)
- Summary showing total files discovered, any errors

### 5.2 Scan with table output

1. Run `voom scan /path/to/media --table`

**Expected:** After scan completes, a table of all discovered files is printed
(path, container, size, track count, hash).

### 5.3 Scan without hashing

1. Run `voom scan /path/to/media --no-hash`

**Expected:** Hashing step is skipped entirely. Discovery and introspection
still occur. Files stored without content hash.

### 5.4 Scan non-recursive

1. Place media files in both `/path/to/media/` and `/path/to/media/subdir/`
2. Run `voom scan /path/to/media --recursive=false`

**Expected:** Only files in the top-level directory are discovered; subdirectory
files are not included.

### 5.5 Scan with worker count

1. Run `voom scan /path/to/media --workers 1`

**Expected:** Scan completes (single-threaded). Compare timing with default
(auto) worker count.

### 5.6 Scan non-existent path

1. Run `voom scan /nonexistent/path`

**Expected:** Error message indicating the path does not exist. Non-zero exit code.

### 5.7 Scan directory with no media files

1. Create an empty directory or one with only non-media files (e.g., `.txt`)
2. Run `voom scan /path/to/empty`

**Expected:** Scan completes with 0 files discovered.

---

## 6. File Inspection (`voom inspect`)

### 6.1 Table output (default)

1. Run `voom inspect /path/to/media/sample.mkv`

**Expected:** Two sections:
- **File info:** path, container format, file size, duration, overall bitrate,
  content hash, internal ID
- **Track table:** columns for track type, codec, language, default flag,
  forced flag, and type-specific details (resolution/fps/HDR for video;
  channels/sample rate for audio)

### 6.2 JSON output

1. Run `voom inspect /path/to/media/sample.mkv --format json`

**Expected:** Valid JSON object containing file metadata and tracks array.
Verify with `| jq .` that it parses cleanly.

### 6.3 Tracks only

1. Run `voom inspect /path/to/media/sample.mkv --tracks-only`

**Expected:** Only the track table is shown; file-level metadata is omitted.

### 6.4 Inspect non-existent file

1. Run `voom inspect /nonexistent/file.mkv`

**Expected:** Error message. Non-zero exit code.

---

## 7. Policy Management (`voom policy`)

### 7.1 Validate a correct policy

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

### 7.2 Validate a policy with errors

1. Create `bad.voom` with a syntax error (e.g., missing closing brace)
2. Run `voom policy validate bad.voom`

**Expected:** Parse or validation error with line/column information.

### 7.3 Validate semantic errors

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

### 7.4 Show compiled policy

1. Run `voom policy show test.voom` (using the valid policy from 7.1)

**Expected:** Displays policy name, config section (languages, on_error),
phases with their dependencies and operations, and full JSON representation
of the compiled policy.

### 7.5 Format a policy

1. Create a policy file with inconsistent indentation and spacing
2. Run `voom policy format messy.voom`

**Expected:** File is reformatted in-place with consistent indentation.
Re-parse produces identical AST (round-trip safe).

### 7.6 List policies

1. Copy one or more `.voom` files into `~/.config/voom/policies/`
2. Run `voom policy list`

**Expected:** Table listing each policy file with name, phase count, and
validation status (OK or ERR).

### 7.7 List with no policies

1. Ensure `~/.config/voom/policies/` is empty
2. Run `voom policy list`

**Expected:** Message indicating no policies found, or an empty table.

---

## 8. Processing (`voom process`)

### 8.1 Dry run

1. Scan a media directory first: `voom scan /path/to/media`
2. Run `voom process /path/to/media --policy test.voom --dry-run`

**Expected:** For each file, the plan of operations is displayed (what would
be kept, removed, reordered, transcoded) without executing any changes. No
files are modified.

### 8.2 Process with backup (default)

1. Run `voom process /path/to/media/single-file.mkv --policy test.voom`

**Expected:**
- Backup of the original file is created before modifications
- Processing progress with worker count displayed
- Summary shows processed/skipped/error counts
- Verify the original file was backed up (check for backup file or directory)

### 8.3 Process without backup

1. Run `voom process /path/to/media/file.mkv --policy test.voom --no-backup`

**Expected:** Processing occurs without creating a backup. File is modified
in-place.

### 8.4 Error strategy: skip

1. Use a policy that will fail on some files
2. Run `voom process /path/to/media --policy test.voom --on-error skip`

**Expected:** Files that cause errors are skipped; remaining files are
processed. Summary shows skip count.

### 8.5 Error strategy: continue

1. Run `voom process /path/to/media --policy test.voom --on-error continue`

**Expected:** Errors are logged but processing continues for all files.

### 8.6 Error strategy: fail

1. Run `voom process /path/to/media --policy test.voom --on-error fail`

**Expected:** Processing stops at the first error. Remaining files are not
processed.

### 8.7 Worker count control

1. Run `voom process /path/to/media --policy test.voom --dry-run --workers 2`

**Expected:** Output indicates 2 workers are used.

### 8.8 Missing policy file

1. Run `voom process /path/to/media --policy nonexistent.voom`

**Expected:** Error message about missing policy file. Non-zero exit code.

---

## 9. Job Management (`voom jobs`)

### 9.1 List jobs (empty)

1. Run `voom jobs list`

**Expected:** Empty table or message indicating no jobs found.

### 9.2 List jobs after processing

1. Run a `voom process` command (from section 8)
2. Run `voom jobs list`

**Expected:** Table with columns: ID (UUID), type, status (color-coded),
progress %, worker ID, creation time. Summary counts by status at the bottom.

### 9.3 Filter jobs by status

1. Run `voom jobs list --status completed`

**Expected:** Only completed jobs are shown.

2. Run `voom jobs list --status failed`

**Expected:** Only failed jobs are shown (or empty if none failed).

### 9.4 Job status detail

1. Note a job ID from `voom jobs list`
2. Run `voom jobs status <job-id>`

**Expected:** Detailed job info: ID, type, status, progress %, message,
error (if any), timestamps (created, started, completed).

### 9.5 Cancel a job

1. Start a long-running process in background or note a pending job ID
2. Run `voom jobs cancel <job-id>`

**Expected:** Confirmation message that job was cancelled.

### 9.6 Invalid job ID

1. Run `voom jobs status 00000000-0000-0000-0000-000000000000`

**Expected:** Error message indicating job not found.

---

## 10. Reports (`voom report`)

### 10.1 Table report

1. Scan media files first (section 5)
2. Run `voom report`

**Expected:** Three sections:
- Total files, total size, total duration
- Container breakdown (count per format, e.g., MKV: 15, MP4: 3)
- Codec breakdown (count per codec, e.g., HEVC: 10, H.264: 5, AAC: 12)

### 10.2 JSON report

1. Run `voom report --format json`

**Expected:** Valid JSON with keys `total_files`, `total_size`, `containers`
(array of objects), `codecs` (array of objects). Verify with `| jq .`

### 10.3 Report with empty library

1. Reset the database: `voom db reset` (confirm with "yes")
2. Run `voom report`

**Expected:** Zeros for all statistics or an appropriate empty-state message.

---

## 11. Database Management (`voom db`)

### 11.1 Prune stale entries

1. Scan a directory, then delete or move some of the scanned media files
2. Run `voom db prune`

**Expected:** Reports count of deleted stale entries (files no longer on disk).

### 11.2 Prune with no stale entries

1. Run `voom db prune` when all scanned files still exist

**Expected:** "No stale entries found" or similar message.

### 11.3 Vacuum database

1. Run `voom db vacuum`

**Expected:** Confirmation message. Database file size may decrease after
pruning + vacuum.

### 11.4 Reset database

1. Run `voom db reset`

**Expected:** Interactive confirmation prompt requiring the user to type "yes".
After confirmation, the database file is deleted and a new empty one is created.

### 11.5 Reset database — decline

1. Run `voom db reset`, type "no" at the prompt

**Expected:** Operation is cancelled. Database is unchanged.

---

## 12. Plugin Management (`voom plugin`)

### 12.1 List plugins

1. Run `voom plugin list`

**Expected:** Table with columns: name, version, capabilities. All native
plugins listed (sqlite-store, tool-detector, discovery, ffprobe-introspector,
policy-evaluator, phase-orchestrator, mkvtoolnix-executor, ffmpeg-executor,
backup-manager, job-manager, web-server). Disabled plugins shown separately.

### 12.2 Plugin info

1. Run `voom plugin info discovery`

**Expected:** Plugin name, version, status (enabled/disabled), and list of
capabilities.

### 12.3 Disable a plugin

1. Run `voom plugin disable web-server`
2. Run `voom plugin list`

**Expected:** web-server appears in the disabled section. Config file is
updated with `disabled_plugins = ["web-server"]`.

### 12.4 Enable a disabled plugin

1. Run `voom plugin enable web-server`
2. Run `voom plugin list`

**Expected:** web-server appears in the enabled section again.

### 12.5 Info for unknown plugin

1. Run `voom plugin info nonexistent-plugin`

**Expected:** Error message indicating plugin not found.

### 12.6 Install WASM plugin

1. Obtain or build a `.wasm` plugin file with a sibling `.toml` manifest
2. Run `voom plugin install /path/to/plugin.wasm`

**Expected:** Plugin and manifest are copied to `~/.config/voom/plugins/wasm/`.
Success message displayed.

### 12.7 Install WASM plugin — missing manifest

1. Create a dummy `.wasm` file without a `.toml` manifest
2. Run `voom plugin install /path/to/dummy.wasm`

**Expected:** Error about missing manifest file.

---

## 13. Web Server (`voom serve`)

### 13.1 Start server (default)

1. Run `voom serve`

**Expected:** Server starts on `http://127.0.0.1:8080`. Startup message
printed with URL.

2. Open `http://127.0.0.1:8080` in a browser.

**Expected:** Dark-themed dashboard page loads with library stats and job
status.

3. Stop the server with Ctrl+C.

### 13.2 Custom host and port

1. Run `voom serve --host 0.0.0.0 --port 9090`

**Expected:** Server starts on `http://0.0.0.0:9090`.

### 13.3 Web pages

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

### 13.4 Security headers

1. Run `curl -I http://127.0.0.1:8080/`

**Expected headers present:**
- `X-Frame-Options: DENY`
- `X-Content-Type-Options: nosniff`
- `Referrer-Policy: strict-origin-when-cross-origin`
- `Content-Security-Policy` header with restrictive policy

---

## 14. REST API

Start the server (`voom serve`) before running these tests. Use `curl` or
any HTTP client.

### 14.1 Files API

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

### 14.2 Jobs API

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

### 14.3 Plugins API

```bash
curl http://127.0.0.1:8080/api/plugins
```
**Expected:** JSON `{"plugins": [{"name": "...", "version": "...", "capabilities": [...]}]}`

### 14.4 Statistics API

```bash
curl http://127.0.0.1:8080/api/stats
```
**Expected:** JSON `{"total_files": N, "total_jobs": [{"status": "...", "count": N}]}`

### 14.5 Tools API

```bash
curl http://127.0.0.1:8080/api/tools
```
**Expected:** JSON listing detected external tools with their versions.

### 14.6 Policy validation API

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

### 14.7 Policy format API

```bash
curl -X POST http://127.0.0.1:8080/api/policy/format \
  -H "Content-Type: application/json" \
  -d '{"source": "policy \"test\" { config { on_error: abort } phase p { container mkv } }"}'
```
**Expected:** `{"formatted": "..."}` with consistently indented output.

### 14.8 Authentication

1. Set `auth_token = "testtoken"` in config.toml
2. Restart `voom serve`
3. `curl http://127.0.0.1:8080/api/files`

**Expected:** 401 Unauthorized.

4. `curl -H "Authorization: Bearer testtoken" http://127.0.0.1:8080/api/files`

**Expected:** 200 OK with file listing.

5. Verify HTML pages (e.g., `curl http://127.0.0.1:8080/`) are still accessible
   without auth.

### 14.9 Server-Sent Events

```bash
curl -N http://127.0.0.1:8080/events
```
**Expected:** Connection stays open. When a scan or process runs concurrently,
events appear as `data: {...}\n\n` lines. If auth is enabled, this endpoint
also requires the Bearer token.

---

## 15. DSL Features (comprehensive policy validation)

These tests validate that the DSL parser and compiler handle all language
constructs correctly. Use `voom policy validate` and `voom policy show` for each.

### 15.1 Minimal policy

```
policy "minimal" {
  config { on_error: abort }
  phase p { container mkv }
}
```
**Expected:** Validates OK. Show output displays 1 phase.

### 15.2 Track filtering with all operators

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

### 15.3 Phase dependencies and conditional execution

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

### 15.4 Transcoding with all options

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

### 15.5 Synthesize block

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

### 15.6 Conditional blocks and rules

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

### 15.7 Plugin metadata field access

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

### 15.8 Full production policy

1. Run `voom policy validate crates/voom-dsl/tests/fixtures/production-normalize.voom`

**Expected:** Validates OK. 6 phases in correct dependency order.

### 15.9 Codec normalization

1. Create a policy using `h265` (alias for `hevc`)
2. Run `voom policy show` on it

**Expected:** Compiled output normalizes `h265` to `hevc`.

### 15.10 Unknown codec detection

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

---

## 16. Shell Completions (`voom completions`)

### 16.1 Bash completions

```bash
voom completions bash > /dev/null
```
**Expected:** Exits 0. Output is valid bash completion script.

### 16.2 Zsh completions

```bash
voom completions zsh > /dev/null
```
**Expected:** Exits 0.

### 16.3 Fish completions

```bash
voom completions fish > /dev/null
```
**Expected:** Exits 0.

---

## 17. Verbose Output

1. Run any command with `-v` flag: `voom -v scan /path/to/media`

**Expected:** Additional info-level log messages appear.

2. Run with `-vv`: `voom -vv doctor`

**Expected:** Debug-level log messages appear.

3. Run with `-vvv`: `voom -vvv status`

**Expected:** Trace-level log messages appear.

---

## 18. Error Handling & Edge Cases

### 18.1 No arguments

1. Run `voom` with no subcommand

**Expected:** Help text displayed. Zero or non-zero exit code depending on
clap configuration.

### 18.2 Unknown subcommand

1. Run `voom foobar`

**Expected:** Error message with suggestion of similar commands. Non-zero
exit code.

### 18.3 Permission denied

1. Run `voom scan /root` (or another directory without read access)

**Expected:** Appropriate error message. Scan reports the inaccessible paths.

### 18.4 Insufficient disk space for backup

1. Process a file on a nearly-full filesystem with backup enabled

**Expected:** Error message about insufficient disk space. File is not
modified (backup-manager validates available space before proceeding).

### 18.5 Large policy input to API

1. Send a POST to `/api/policy/validate` with a body exceeding 1 MiB

**Expected:** Rejected with appropriate error (request too large).

---

## 19. End-to-End Workflow

This test validates a complete real-world workflow from start to finish.

1. **Initialize:** `voom init`
2. **Check health:** `voom doctor` — all required tools OK
3. **Scan library:** `voom scan /path/to/media --table`
   - Verify file count matches expected number of media files
4. **Inspect a file:** `voom inspect /path/to/media/sample.mkv`
   - Verify tracks match what ffprobe reports
5. **Check status:** `voom status` — file count matches scan
6. **Create a policy:** Write a `.voom` file with normalize + transcode phases
7. **Validate policy:** `voom policy validate policy.voom` — OK
8. **Dry run:** `voom process /path/to/media --policy policy.voom --dry-run`
   - Review plans for each file; verify they are sensible
9. **Process one file:** `voom process /path/to/media/sample.mkv --policy policy.voom`
   - Verify backup was created
   - Verify output file has expected tracks/container
10. **Check jobs:** `voom jobs list` — shows completed job for the file
11. **Generate report:** `voom report` — stats reflect the processed file
12. **Start web server:** `voom serve` (in background)
13. **Verify API:** `curl http://127.0.0.1:8080/api/files` — returns the processed file
14. **Verify dashboard:** Open `http://127.0.0.1:8080/` — shows updated stats
15. **Cleanup:** `voom db prune && voom db vacuum`
