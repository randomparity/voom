# VOOM

[![CI](https://github.com/randomparity/voom/actions/workflows/ci.yml/badge.svg)](https://github.com/randomparity/voom/actions/workflows/ci.yml)
[![Release](https://github.com/randomparity/voom/actions/workflows/release.yml/badge.svg)](https://github.com/randomparity/voom/actions/workflows/release.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org/)

**Policy-driven video library management, built in Rust.**

VOOM automatically normalizes, organizes, and processes video files according
to declarative policies you write in a purpose-built DSL. Describe what your
library should look like and VOOM figures out how to get there.

## Features

### Declarative Policy Language

Write `.voom` policy files that describe outcomes, not commands. VOOM's custom
DSL supports phased pipelines with dependency ordering, conditional logic,
track filtering, audio synthesis, transcoding, and metadata enrichment -- all
in a readable block syntax:

```
policy "english-optimized" {
  phase containerize {
    container mkv
  }

  phase transcode {
    depends_on: [containerize]
    skip when video.codec == "hevc"

    transcode video to hevc {
      crf: 20, preset: medium
      hw: auto, hw_fallback: true
    }
  }

  phase audio {
    depends_on: [transcode]
    keep audio where lang == eng and not commentary
    synthesize "AAC Stereo" {
      codec: aac, channels: stereo
      source: prefer(lang == eng and not commentary)
      bitrate: "192k"
    }
  }

  phase subtitles {
    depends_on: [transcode]
    keep subtitles where lang == eng and not commentary
  }
}
```

Phases run in dependency order with `skip when` / `run_if` guards, `on_error`
handling, and `rules` blocks for multi-branch decision logic. 10 example
policies ship in `docs/examples/` covering anime, archival, transcoding,
metadata enrichment, and more.

### Hardware-Accelerated Transcoding

Four GPU backends with automatic detection and software fallback:

| Backend | Platform | Codecs |
|---------|----------|--------|
| **NVENC** | NVIDIA GPUs | H.264, HEVC, AV1 |
| **Quick Sync** | Intel iGPUs | H.264, HEVC, AV1, VP9 |
| **VA-API** | Linux (AMD/Intel) | H.264, HEVC, AV1, VP9 |
| **VideoToolbox** | macOS | H.264, HEVC |

Set `hw: auto` in any `transcode` block and VOOM picks the best available
backend. `hw_fallback: true` drops to software encoding if hardware fails at
runtime. Per-device validation ensures only working encoders are selected.

### Subtitle Intelligence

Full subtitle pipeline from filtering to AI-powered generation:

- **Filter and organize** -- keep/remove subtitles by language, codec, forced/default flags, title patterns, or regex
- **Bulk metadata** -- clear defaults, set forced flags, assign languages across all subtitle tracks
- **AI subtitle generation** (WASM plugins) -- Whisper-based transcription with automatic foreign-language segment detection, forced subtitle SRT output, and content-hash caching

### Health Checks and Diagnostics

`voom health check` (or `voom doctor`) validates your entire environment:

- **External tools** -- detects ffmpeg, ffprobe, mkvmerge, mkvpropedit, mkvextract, mediainfo, HandBrakeCLI with version info
- **GPU hardware** -- enumerates devices, validates each hardware encoder per-device, reports VRAM
- **Database** -- bootstraps and verifies SQLite connectivity
- **Config** -- checks for valid configuration files
- **Plugins** -- lists all registered plugins and their status
- **Filesystem** -- periodic writability probes with configurable intervals

Health history is stored in SQLite for trend analysis via `voom health history`.

### Web Dashboard

`voom serve` starts an htmx + Alpine.js web UI with:

- Library browser with filtering by container, codec, language, and path
- Real-time job monitoring via Server-Sent Events
- In-browser policy editor with validation and formatting
- Full REST API for automation (`/api/files`, `/api/jobs`, `/api/stats`, ...)
- Optional bearer-token auth, rate limiting, CSP headers

### Job System

Priority-based job queue backed by SQLite with configurable parallel workers:

- Crash recovery -- jobs survive restarts
- Per-file approval mode (`--approve`) for manual review
- Dry-run and plan-only modes for previewing changes
- Cancel, retry, and bulk cleanup via CLI or API

### Backup and Recovery

Automatic pre-modification backups with two storage modes:

- **Sibling** -- `.voom-backup/` next to the original file (timestamped `.vbak`)
- **Global** -- single backup directory with UUID-prefixed names

Disk space validation before copy. Restore any backup with `voom backup restore`.

### Library Reporting

`voom report` with deep library analytics:

- **Default** -- file counts, total size/duration, container and codec breakdowns
- **`--stats`** -- video resolution/HDR/VFR distributions, audio language/codec/channel layouts, subtitle type counts, processing time and size savings
- **`--issues`** -- safeguard violations grouped by kind, phase, and message
- **`--plans`** -- per-phase processing summaries with skip reasons
- **`--history`** -- snapshot trend analysis over time

All reports output as `table`, `json`, or `plain`.

### Plugin Architecture

Two-tier plugin model around a thin kernel with zero media knowledge:

- **Native plugins** -- compiled into the binary (discovery, introspection, storage, executors, backup, job management, health checks, web UI)
- **WASM plugins** -- sandboxed, language-agnostic extensions via wasmtime (Whisper transcription, subtitle generation, Radarr/Sonarr metadata, HandBrake executor, audio language detection)

Plugins communicate exclusively through a priority-ordered event bus. Install
third-party WASM plugins with `voom plugin install <path.wasm>`.

## Quick Start

```bash
# Build
cargo build

# Run tests (~800+)
cargo test

# CLI help
cargo run -- --help

# Validate a policy
cargo run -- policy validate docs/examples/english-optimized.voom

# Scan and process a library
cargo run -- scan /path/to/videos
cargo run -- process /path/to/videos --policy docs/examples/english-optimized.voom

# Check your environment
cargo run -- health check

# Start the web UI
cargo run -- serve
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `scan` | Discover and introspect video files |
| `inspect` | Show detailed file information |
| `process` | Apply a policy to files |
| `policy` | Validate, format, diff, and list policies |
| `files` | List and search library files |
| `plans` | View and manage saved plans |
| `report` | Library analytics and issue reporting |
| `jobs` | Manage the processing queue |
| `backup` | List, restore, and clean up backups |
| `health` | Environment diagnostics and history |
| `doctor` | Hidden alias for `health check` (kept for compatibility) |
| `serve` | Start the web dashboard |
| `events` | View and tail the event log |
| `db` | Database maintenance (prune, vacuum, reset) |
| `config` | View and edit configuration |
| `tools` | Detect and report external tool availability |
| `history` | Show per-file processing history |
| `plugin` | Manage native and WASM plugins |
| `init` | Scaffold a starter policy and config |
| `completions` | Generate shell completions (bash/zsh/fish) |

## Architecture

```
.voom policy --> pest parser --> AST --> CompiledPolicy
                                              |
    discovery (walkdir) --> FileDiscovered --> event bus
    ffprobe introspection --> FileIntrospected --> SQLite
                                              |
    policy evaluator --> Plan structs --> executor (FFmpeg / MKVToolNix)
```

All functionality lives in plugins. The kernel provides only the event bus and
plugin registry. See [`docs/architecture.md`](docs/architecture.md) for details.

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `voom-cli` | CLI binary with subcommands |
| `voom-kernel` | Event bus, plugin registry, native + WASM loader |
| `voom-domain` | Shared types (`MediaFile`, `Track`, `Plan`, `Event`) |
| `voom-dsl` | PEG parser, AST, compiler, validator, formatter |
| `voom-process` | Subprocess utilities with timeout-aware execution |
| `voom-wit` | WIT interface definitions for WASM plugins |
| `voom-plugin-sdk` | SDK for third-party WASM plugin authors |

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).
You may use, modify, and distribute this software under the terms of the AGPL-3.0.
Any network-accessible deployment of modified versions must make the complete
source code available to users under the same license.
