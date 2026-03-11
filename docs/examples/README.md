# VOOM Example Policies

Sample `.voom` policy files demonstrating DSL features and plugin usage. All examples are parser-verified by integration tests.

## Examples

### [minimal.voom](minimal.voom)
The simplest valid policy — a single phase that converts files to MKV container.

### [movie-library.voom](movie-library.voom)
Standard movie library normalization. Config block, track filtering (keep/remove with where clauses), track ordering, defaults, and validation checks.

**Plugins used:** discovery, ffprobe-introspector, sqlite-store, policy-evaluator, phase-orchestrator, mkvtoolnix-executor, backup-manager

### [anime-collection.voom](anime-collection.voom)
Multi-language anime library with Japanese/English handling. Forced subtitle detection, `is_dubbed`/`is_original` predicates, `audio_is_multi_language`, `when`/`else` blocks, `rules all` block, `count()` conditions, `title contains` filters, and sonarr-metadata integration.

**Plugins used:** all native + sonarr-metadata (WASM)

### [transcode-hevc.voom](transcode-hevc.voom)
HEVC transcoding pipeline with hardware acceleration. `skip when` with field access, video/audio transcode settings, `synthesize` with all options (codec, channels, source, bitrate, skip_if_exists, create_if, title, language, position), `run_if` conditional phases.

**Plugins used:** ffmpeg-executor, mkvtoolnix-executor, backup-manager

### [metadata-enrichment.voom](metadata-enrichment.voom)
External metadata enrichment using WASM plugin data. `field exists` conditions, `set_language` with field access, `set_tag` with field and literal values, `rules first` mode, `skip` action, `is_original`/`is_dubbed` predicates, all comparison operators.

**Plugins used:** radarr-metadata, sonarr-metadata (WASM), mkvtoolnix-executor

### [strict-archive.voom](strict-archive.voom)
Strict archival policy with aggressive validation. `fail` action (halts processing), all comparison operators, `count()` with various operators, `title matches` (regex patterns), `on_error: abort`, exhaustive track filtering with complex boolean logic.

**Plugins used:** all native plugins, backup-manager (mandatory backups)

### [full-pipeline.voom](full-pipeline.voom)
Comprehensive reference exercising **every DSL construct**. Not intended for production — serves as a living test that all language features work together. 10 phases covering container conversion, track cleanup, metadata, ordering, transcoding, synthesis, conditional logic, rules, and plugin metadata enrichment.

**Plugins used:** all native + all WASM (radarr-metadata, sonarr-metadata, whisper-transcriber, audio-synthesizer, handbrake-executor)

## DSL Feature Coverage

| Feature | Examples |
|---------|----------|
| `config` block | movie-library, anime, transcode, metadata, strict, full |
| `depends_on` | all except minimal |
| `skip when` | transcode, full |
| `run_if` (modified/completed) | anime, transcode, strict, full |
| `on_error` | movie-library, transcode, strict, full |
| `container` | minimal, movie-library, anime, transcode, full |
| `keep` / `remove` | movie-library, anime, strict, full |
| `order tracks` | movie-library, anime, strict, full |
| `defaults` | movie-library, anime, strict, full |
| `actions` (video/audio/subtitle) | movie-library, anime, strict, full |
| `transcode` (video/audio) | transcode, full |
| `synthesize` | transcode, full |
| `when` / `else` | anime, metadata, full |
| `rules first` | metadata, full |
| `rules all` | anime, full |
| `exists()` | anime, transcode, full |
| `count()` | movie-library, anime, strict, full |
| `audio_is_multi_language` | anime, full |
| `is_dubbed` / `is_original` | anime, metadata, full |
| `field exists` | metadata, full |
| `field compare` | transcode, strict, full |
| `set_default` / `set_forced` | anime, full |
| `set_language` (field) | metadata, full |
| `set_tag` | metadata, full |
| `skip` / `fail` / `warn` | movie-library, anime, strict, metadata, full |
| `title contains` / `matches` | anime, strict, full |
| `lang ==` / `codec ==` | anime, strict, full |
| `channels` comparison | anime, strict, full |
| `commentary` / `forced` / `default` / `font` | movie-library, anime, strict, full |
| Boolean logic (and/or/not/parens) | all except minimal |
