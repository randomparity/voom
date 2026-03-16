# VOOM Quickstart Guide

## Prerequisites

- **Rust** (2021 edition, stable toolchain)
- **ffprobe** and **ffmpeg** (for media introspection and transcoding)
- **mkvtoolnix** (mkvpropedit + mkvmerge, for MKV operations)
- Optional: **mediainfo** (additional metadata)

Verify your tools:

```bash
cargo --version
ffprobe -version
ffmpeg -version
mkvmerge --version
```

## Building from Source

```bash
git clone https://github.com/your-org/voom.git
cd voom
cargo build --release
```

The binary is at `target/release/voom`.

## First-Time Setup

```bash
# Initialize config directory and default settings
voom init

# Check that external tools are detected
voom doctor
```

This creates `~/.config/voom/config.toml` with default settings and a starter policy at `~/.config/voom/policies/default.voom`.

## Configuration

Edit `~/.config/voom/config.toml`:

```toml
# Where VOOM stores its database and data files
data_dir = "/home/you/.config/voom"

# Optional Bearer token for authenticating API and SSE requests
# auth_token = "your-secret-token"

[plugins]
# Directory for WASM plugins (default: ~/.config/voom/plugins/wasm/)
# wasm_dir = "/path/to/wasm/plugins"

# Plugins to disable by name
disabled_plugins = []
```

Or use the CLI:

```bash
voom config show
voom config edit    # Opens in $EDITOR
```

## Core Workflow

### 1. Scan Your Library

Discover and introspect media files:

```bash
# Scan a directory recursively
voom scan /path/to/media -r

# Scan with multiple workers and no content hashing (faster)
voom scan /path/to/media -r --workers 8 --no-hash
```

### 2. Inspect Files

View metadata for individual files:

```bash
# Table format (default)
voom inspect /path/to/movie.mkv

# JSON output
voom inspect /path/to/movie.mkv --format json

# Tracks only
voom inspect /path/to/movie.mkv --tracks-only
```

### 3. Write a Policy

Create a `.voom` policy file. Here's a minimal example:

```
policy "my-normalize" {
  config {
    languages audio: [eng, und]
    languages subtitle: [eng]
    on_error: continue
  }

  phase normalize {
    keep audio where lang in [eng, und]
    keep subtitles where lang in [eng] and not commentary
    remove attachments where not font

    order tracks [video, audio_main, subtitle_main, attachment]

    defaults {
      audio: first_per_language
      subtitle: none
    }
  }
}
```

Save as `my-normalize.voom`.

### 4. Validate Your Policy

```bash
# Check for errors
voom policy validate my-normalize.voom

# Auto-format the policy file
voom policy format my-normalize.voom

# Show the parsed policy structure
voom policy show my-normalize.voom
```

### 5. Dry Run

Preview what changes would be made without modifying files:

```bash
voom process /path/to/media --policy my-normalize.voom --dry-run
```

### 6. Process Files

Apply the policy:

```bash
# Process with backup (default)
voom process /path/to/media --policy my-normalize.voom

# Process with multiple workers
voom process /path/to/media --policy my-normalize.voom --workers 4

# Process requiring approval per file
voom process /path/to/media --policy my-normalize.voom --approve

# Process without backup (use with caution)
voom process /path/to/media --policy my-normalize.voom --no-backup
```

### 7. Monitor Jobs

```bash
# List all jobs
voom jobs list

# Filter by status
voom jobs list --status running

# Check a specific job
voom jobs status <job-id>

# Cancel a job
voom jobs cancel <job-id>
```

### 8. View Reports

```bash
voom report
voom report --format json
```

## Web Dashboard

Start the web UI:

```bash
voom serve
voom serve --port 9090 --host 0.0.0.0
```

Then open `http://localhost:8080` in your browser. The dashboard shows:
- Library statistics and recent activity
- File browser with search and filtering
- Policy editor with syntax highlighting and live validation
- Job monitor with real-time progress (via SSE)
- Plugin manager
- System settings

## Writing Policies

### Basic Structure

Every policy has a name, optional config, and one or more phases:

```
policy "name" {
  config { ... }       // optional global settings

  phase phase_name {
    // operations
  }
}
```

### Phase Control

Phases can depend on other phases and have conditional execution:

```
phase transcode {
  depends_on: [normalize]         // run after normalize
  skip when video.codec == hevc   // skip if already HEVC
  run_if normalize.modified       // only if normalize changed something
  on_error: continue              // continue on errors (default: abort)

  // operations...
}
```

### Track Operations

```
keep audio where lang in [eng, jpn] and not commentary
keep subtitles where lang in [eng] and forced
remove attachments where not font

order tracks [video, audio_main, audio_alternate, subtitle_main, attachment]

defaults {
  audio: first_per_language
  subtitle: none
}
```

### Transcoding

```
transcode video to hevc {
  crf: 20
  preset: medium
  hw: auto
  hw_fallback: true
}

transcode audio to aac {
  preserve: [truehd, dts_hd, flac]
  bitrate: 192k
}
```

### Audio Synthesis

```
synthesize "Stereo AAC" {
  codec: aac
  channels: stereo
  source: prefer(codec in [truehd, dts_hd, flac] and channels >= 6)
  bitrate: 192k
  skip_if_exists { codec in [aac] and channels == 2 }
  title: "Stereo (AAC)"
  language: inherit
  position: after_source
}
```

### Conditional Logic

```
when exists(audio where lang == jpn) and not exists(subtitle where lang == eng) {
  warn "Japanese audio but no English subtitles"
}

rules first {
  rule "multi-language check" {
    when audio_is_multi_language {
      warn "Multiple audio languages detected"
    }
  }
}
```

See [DSL Language Reference](dsl-reference.md) for the complete language specification.

## Plugin Management

```bash
# List all plugins (native + WASM)
voom plugin list

# Show plugin details
voom plugin info discovery

# Enable/disable plugins
voom plugin enable radarr-metadata
voom plugin disable whisper-transcriber

# Install a WASM plugin
voom plugin install /path/to/my-plugin.wasm
```

## Handling Bad Files

When a file fails introspection (corrupt, unreadable, etc.), it is recorded in the database as a "bad file". On subsequent runs, bad files are automatically skipped to avoid re-failing.

```bash
# See which files are bad and why
voom db list-bad

# Re-attempt introspection on bad files
voom process /path/to/media --policy normalize.voom --force-rescan

# Remove bad file entries from DB (after manually fixing files)
voom db purge-bad

# Delete bad files from disk and DB
voom db clean-bad --yes
```

The `voom status` command shows the bad file count when any exist.

## Database Maintenance

```bash
# Remove entries for files that no longer exist
voom db prune

# Optimize the database
voom db vacuum

# Reset the database (destructive!)
voom db reset
```

## Shell Completions

Generate shell completions for your shell:

```bash
# Bash
voom completions bash > ~/.local/share/bash-completion/completions/voom

# Zsh
voom completions zsh > ~/.zfunc/_voom

# Fish
voom completions fish > ~/.config/fish/completions/voom.fish
```

## Library Status

Check the current state of your library and daemon:

```bash
voom status
```

## Next Steps

- Read the [DSL Language Reference](dsl-reference.md) for full policy syntax
- Read the [Plugin Development Guide](plugin-development.md) to write custom plugins
- Read the [CLI Reference](cli-reference.md) for all commands and options
- Read the [Architecture Overview](architecture.md) for system design details
