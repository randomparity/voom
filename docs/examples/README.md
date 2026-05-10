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
HEVC transcoding pipeline with hardware acceleration. `skip when` with field access, video/audio transcode settings, auto-crop tuning, `synthesize` with all options (codec, channels, source, bitrate, skip_if_exists, create_if, title, language, position), `run_if` conditional phases.

**Plugins used:** ffmpeg-executor, mkvtoolnix-executor, backup-manager

### [containerize-then-transcode.voom](containerize-then-transcode.voom)
Regression-focused multi-phase policy that remuxes to MKV before transcoding
video. Useful for verifying that a container path change and a downstream
transcode share the same persisted file identity.

**Plugins used:** mkvtoolnix-executor, ffmpeg-executor, backup-manager

### [continue-on-error-transcode.voom](continue-on-error-transcode.voom)
Batch-oriented transcode policy with `on_error: continue`. Useful for testing
that failed executable plans are visible in job accounting and summaries while
the rest of the batch continues.

**Plugins used:** ffmpeg-executor, backup-manager

### [hw-nvenc-hevc.voom](hw-nvenc-hevc.voom)
Explicit NVENC HEVC transcode policy. Pair with
`[plugin.ffmpeg-executor] nvenc_max_parallel` to validate hardware resource
limiting independently from `voom process --workers`.

**Plugins used:** ffmpeg-executor, backup-manager

### [transcode-video-drop-attachments.voom](transcode-video-drop-attachments.voom)
Attachment-safe video transcode policy. Demonstrates that ffmpeg video
transcodes exclude Matroska attachments from the output mapping so image
attachments cannot become PNG/JPEG video streams.

**Plugins used:** ffmpeg-executor, mkvtoolnix-executor, backup-manager

### [preflight-archive.voom](preflight-archive.voom)
Archival policy for pre-flight cost estimates. Demonstrates container,
video-transcode, and audio-transcode phases intended for `voom process --estimate`.

**Plugins used:** policy-evaluator, ffmpeg-executor

### [preflight-size-gate.voom](preflight-size-gate.voom)
Transcode policy designed for `voom process --confirm-savings`, so low-value
transcodes can be skipped before executor dispatch.

**Plugins used:** policy-evaluator, ffmpeg-executor

### [remote-backup-transcode.voom](remote-backup-transcode.voom)
Destructive transcode policy intended for remote-backup testing. Pair with
`remote-backup-rclone.toml` or `remote-backup-s3.toml` in `~/.config/voom/config.toml`
to upload and verify backups before mutation.

**Plugins used:** policy-evaluator, ffmpeg-executor, backup-manager

### [hdr-archival.voom](hdr-archival.voom)
HDR archival transcode policy. Preserves detected HDR10 metadata while encoding HEVC output and demonstrates optional hardware acceleration.

**Plugins used:** ffmpeg-executor, mkvtoolnix-executor, backup-manager

### [hdr10plus-preserve.voom](hdr10plus-preserve.voom)
HDR10+ archival transcode policy. Requires `hdr10plus_tool` when the source has HDR10+ dynamic metadata.

**Plugins used:** ffmpeg-executor, mkvtoolnix-executor, backup-manager

### [dolby-vision-rpu.voom](dolby-vision-rpu.voom)
Dolby Vision archival transcode policy. Preserves RPU metadata for supported profiles 5, 7, and 8 when `dovi_tool` is available.

**Plugins used:** ffmpeg-executor, mkvtoolnix-executor, backup-manager

### [hdr-sdr-mobile.voom](hdr-sdr-mobile.voom)
Mobile-oriented SDR derivative policy. Tone-maps HDR sources to BT.709 SDR output, downscales video, and creates stereo AAC audio.

**Plugins used:** ffmpeg-executor, backup-manager

### [metadata-enrichment.voom](metadata-enrichment.voom)
External metadata enrichment using WASM plugin data. `field exists` conditions, `set_language` with field access, `set_tag` with field and literal values, `rules first` mode, `skip` action, `is_original`/`is_dubbed` predicates, all comparison operators.

**Plugins used:** radarr-metadata, sonarr-metadata (WASM), mkvtoolnix-executor

### [speech-language-filter.voom](speech-language-filter.voom)
Generated speech corpus language filtering. Demonstrates keeping English and
mixed-language speech tracks while warning on Spanish-only files.

**Plugins used:** policy-evaluator

### [speech-transcription-check.voom](speech-transcription-check.voom)
Declared-language speech fixture workflow. Demonstrates tagging generated TTS
speech files by declared audio language and validating that speech audio remains.

**Plugins used:** policy-evaluator

### [attachment-management.voom](attachment-management.voom)
Attachment management for font and image attachments. Demonstrates `remove attachments where not font`, `keep attachments where` with compound filters, `count(attachments)` and `exists(attachments)` conditions, `title contains` for cover art detection. Common use case: anime fansub font cleanup.

**Plugins used:** discovery, ffprobe-introspector, sqlite-store, policy-evaluator, phase-orchestrator, mkvtoolnix-executor

### [strict-archive.voom](strict-archive.voom)
Strict archival policy with aggressive validation. `fail` action (halts processing), all comparison operators, `count()` with various operators, `title matches` (regex patterns), `on_error: abort`, exhaustive track filtering with complex boolean logic.

**Plugins used:** all native plugins, backup-manager (mandatory backups)

### [full-pipeline.voom](full-pipeline.voom)
Comprehensive reference exercising **every DSL construct**. Not intended for production — serves as a living test that all language features work together. 10 phases covering container conversion, track cleanup, metadata, ordering, transcoding, synthesis, conditional logic, rules, and plugin metadata enrichment.

**Plugins used:** all native + all WASM (radarr-metadata, sonarr-metadata, whisper-transcriber, audio-synthesizer, handbrake-executor)

## DSL Feature Coverage

| Feature | Examples |
|---------|----------|
| `config` block | movie-library, anime, transcode, metadata, strict, full, speech-language-filter, speech-transcription-check |
| `depends_on` | all except minimal |
| `skip when` | transcode, full |
| `run_if` (modified/completed) | anime, transcode, strict, full |
| `on_error` | movie-library, transcode, continue-on-error-transcode, strict, full, speech-language-filter, speech-transcription-check |
| `container` | minimal, movie-library, anime, transcode, full |
| `keep` / `remove` | movie-library, anime, attachment, strict, full, speech-language-filter |
| `order tracks` | movie-library, anime, strict, full |
| `defaults` | movie-library, anime, strict, full |
| `actions` (video/audio/subtitle) | movie-library, anime, strict, full |
| `transcode` (video/audio) | transcode, containerize-then-transcode, hw-nvenc-hevc, transcode-video-drop-attachments, preflight-archive, preflight-size-gate, hdr-archival, hdr-sdr-mobile, full |
| `preserve_hdr` / `tonemap` | hdr-archival, hdr10plus-preserve, dolby-vision-rpu, hdr-sdr-mobile |
| `crop: auto` | transcode |
| `synthesize` | transcode, full |
| `when` / `else` | anime, metadata, full |
| `rules first` | metadata, full |
| `rules all` | anime, full, speech-transcription-check |
| `exists()` | anime, attachment, transcode, full, speech-language-filter, speech-transcription-check |
| `count()` | movie-library, anime, attachment, strict, full, speech-transcription-check |
| `audio_is_multi_language` | anime, full |
| `is_dubbed` / `is_original` | anime, metadata, full |
| `field exists` | metadata, full |
| `field compare` | transcode, strict, full |
| `set_default` / `set_forced` | anime, full |
| `set_language` (field) | metadata, full |
| `set_tag` | metadata, full, speech-transcription-check |
| `skip` / `fail` / `warn` | movie-library, anime, strict, metadata, full, speech-language-filter, speech-transcription-check |
| `title contains` / `matches` | anime, attachment, strict, full |
| `lang ==` / `codec ==` | anime, strict, full, speech-language-filter, speech-transcription-check |
| `channels` comparison | anime, strict, full |
| `commentary` / `forced` / `default` / `font` | movie-library, anime, attachment, strict, full |
| Boolean logic (and/or/not/parens) | all except minimal |
