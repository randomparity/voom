# VOOM DSL Language Reference

VOOM policies are written in `.voom` files using a custom block-based DSL (Domain-Specific Language). The language is designed specifically for declaring video library processing rules.

## Syntax Overview

- **Block-based** — Curly braces `{ }` delimit blocks
- **Comments** — Line comments with `//`
- **Whitespace** — Whitespace and newlines are not significant (free-form)
- **No semicolons** — Statements are delimited by structure, not punctuation
- **Identifiers** — Start with a letter or `_`, may contain letters, digits, `_`, and `-`

## Policy Structure

Every `.voom` file defines exactly one policy:

```
policy "<name>" {
  config { ... }           // optional — global settings
  phase <name> { ... }     // one or more phases
}
```

### Example

```
policy "my-normalize" {
  config {
    languages audio: [eng, und]
    on_error: continue
  }

  phase containerize {
    container mkv
  }

  phase normalize {
    depends_on: [containerize]
    keep audio where lang in [eng, jpn]
    remove attachments where not font
  }
}
```

## Config Block

The optional `config` block sets policy-wide defaults.

```
config {
  languages audio: [eng, und]                          // allowed audio languages
  languages subtitle: [eng, und]                       // allowed subtitle languages
  commentary_patterns: ["commentary", "director"]      // patterns to detect commentary tracks
  on_error: continue                                   // error handling: abort | continue | skip
}
```

| Setting | Type | Description |
|---------|------|-------------|
| `languages audio` | list | Allowed audio language codes (ISO 639-2/B) |
| `languages subtitle` | list | Allowed subtitle language codes |
| `commentary_patterns` | list of strings | Title patterns that classify a track as commentary |
| `on_error` | identifier | Global error handling strategy |

## Phases

Phases are the primary organizational unit. They execute sequentially (respecting dependencies) and each contains a set of operations.

```
phase <name> {
  // control directives (optional)
  depends_on: [phase1, phase2]
  skip when <condition>
  run_if <phase>.<event>
  on_error: <strategy>

  // operations
  ...
}
```

### Phase Control Directives

#### `depends_on`

Declares that this phase must run after the listed phases. The kernel uses topological sort to determine execution order.

```
depends_on: [containerize, normalize]
```

#### `skip when`

Skip this entire phase if the condition is true.

```
skip when video.codec in [hevc, h265]
skip when exists(audio where codec == aac)
```

#### `run_if`

Only run this phase if a prior phase had a specific outcome.

```
run_if transcode.modified       // run only if transcode changed the file
run_if normalize.completed      // run only if normalize finished (even with no changes)
```

#### `on_error`

Per-phase error handling strategy (overrides config-level `on_error`):

```
on_error: abort      // stop all processing (default)
on_error: continue   // log and continue with next file
on_error: skip       // skip remaining phases for this file
```

## Track Operations

### `keep`

Keep tracks matching the filter; remove all others of that type.

```
keep audio where lang in [eng, jpn, und]
keep subtitles where lang in [eng] and not commentary
keep video    // keep all video tracks
```

### `remove`

Remove tracks matching the filter.

```
remove attachments where not font
remove audio where commentary
remove subtitles where lang in [chi, kor]
```

### Track Targets

Both `keep` and `remove` operate on a track target:

| Target | Description |
|--------|-------------|
| `audio` | Audio tracks |
| `subtitle` / `subtitles` | Subtitle tracks (both spellings accepted) |
| `video` | Video tracks |
| `attachment` / `attachments` | Attachment tracks (both spellings accepted) |
| `track` | Any track type (wildcard; valid in `exists()` and `count()` queries) |

### `order tracks`

Specify the track ordering within the output file.

```
order tracks [
  video, audio_main, audio_alternate,
  subtitle_main, subtitle_forced,
  audio_commentary, subtitle_commentary, attachment
]
```

Track type identifiers for ordering:

| Identifier | Description |
|------------|-------------|
| `video` | Video tracks |
| `audio_main` | Primary audio tracks |
| `audio_alternate` | Alternate language audio |
| `audio_commentary` | Commentary audio |
| `subtitle_main` | Primary subtitles |
| `subtitle_forced` | Forced subtitles |
| `subtitle_commentary` | Commentary subtitles |
| `attachment` | Attachments |
| `audio` | All audio tracks (unclassified) |
| `subtitle` / `subtitles` | All subtitle tracks (unclassified) |

### `defaults`

Set default track selection behavior.

```
defaults {
  audio: first_per_language      // first track of each language gets default flag
  subtitle: none                 // no default subtitle
}
```

Default strategies:

| Strategy | Description |
|----------|-------------|
| `first_per_language` | First track of each language is marked default |
| `none` | No tracks marked as default |
| `first` | Only the first track is marked default |
| `all` | All tracks of this type are marked as default |

### Track Actions

Bulk operations on track metadata flags.

```
audio actions {
  clear_all_default: true
  clear_all_forced: true
  clear_all_titles: true
}

subtitle actions {
  clear_all_default: true
  clear_all_forced: true
}

video actions {
  clear_all_titles: true
}
```

## Container Operation

Set the output container format.

```
container mkv
container mp4
```

If the file is not already in the target format, it will be remuxed (no re-encoding).

## Container Metadata Operations

### `clear_tags`

Remove all container-level metadata tags from the file.

```
clear_tags
```

### `set_tag`

Set a container-level metadata tag to a literal value or a plugin field reference.

```
set_tag "title" "My Movie"
set_tag "title" plugin.radarr.title
```

### `delete_tag`

Remove a specific container-level metadata tag.

```
delete_tag "encoder"
delete_tag "creation_time"
```

> **Ordering note:** The validator reports an error if `set_tag` appears before `clear_tags`
> in the same phase, because `clear_tags` would overwrite the tag just set.

## Transcode Operations

### Video Transcode

```
transcode video to hevc {
  crf: 20                    // quality (lower = better, 0-51 for x265)
  preset: medium             // encoding speed/quality tradeoff
  max_resolution: 1080p      // downscale if above this resolution
  scale_algorithm: lanczos   // scaling algorithm
  hdr_mode: preserve         // HDR handling: preserve | tonemap
  tune: film                 // encoder tuning: film | animation | grain | ...
  hw: auto                   // hardware acceleration: auto | nvenc | qsv | vaapi | none
  hw_fallback: true          // fall back to software if HW fails
  crop: auto                 // detect and remove black bars before encoding
}
```

Auto-crop runs FFmpeg's `cropdetect` filter before the transcode, caches the
detected crop rectangle on the file record, and reuses the cached result on
later runs. Crop values are stored as pixels removed from the left, top, right,
and bottom edges.

| Setting | Type | Description |
|---------|------|-------------|
| `crop` | identifier | Enable automatic crop detection. The only supported value is `auto`. |
| `crop_sample_duration` | integer | Seconds to inspect per sample. Default: `60`. |
| `crop_sample_count` | integer | Number of samples across the file. Default: `3`. |
| `crop_threshold` | integer | FFmpeg cropdetect luma threshold, `0`-`255`. Default: `24`. |
| `crop_minimum` | integer | Ignore edge crops smaller than this many pixels. Default: `4`. |
| `crop_preserve_bottom_pixels` | integer | Reduce the detected bottom crop by this many pixels, useful for subtitles or captions near the lower edge. Default: `0`. |
| `crop_aspect_lock` | list | Optional ratio strings such as `["16/9", "4/3"]`; VOOM expands the crop to the closest reachable ratio without cropping deeper. |

Example:

```
transcode video to hevc {
  crf: 20
  crop: auto
  crop_sample_duration: 60
  crop_sample_count: 3
  crop_threshold: 24
  crop_minimum: 4
  crop_preserve_bottom_pixels: 60
  crop_aspect_lock: ["16/9", "4/3"]
}
```

### Audio Transcode

```
transcode audio to aac {
  preserve: [truehd, dts_hd, flac]   // don't transcode these codecs
  bitrate: 192k                       // target bitrate
  channels: stereo                    // channel layout (stereo, 5.1, etc.)
                                      // use "preserve" to keep original layout (default)
}
```

### Audio Loudness Normalization

```
keep audio where lang == eng and not commentary {
  normalize: ebu_r128 {
    target_lufs: -23
    true_peak_db: -1.0
    lra_max: 18
  }
}
```

`normalize` uses ffmpeg `loudnorm` in EBU R128 mode. Supported presets are
`ebu_r128`, `ebu_r128_broadcast`, `streaming_movies`, `streaming_music`,
`mobile`, and `voice_focused`.

## Synthesize Operation

Create new audio tracks from existing ones.

```
synthesize "Stereo AAC" {
  codec: aac                                                          // output codec
  channels: stereo                                                    // channel layout
  source: prefer(codec in [truehd, dts_hd, flac] and channels >= 6)  // source preference
  bitrate: "192k"                                                     // target bitrate
  skip_if_exists { codec in [aac] and channels == 2 and not commentary }  // skip condition
  create_if <condition>                                                // creation condition
  title: "Stereo (AAC)"                                               // track title
  language: inherit                                                    // inherit from source
  position: after_source                                               // track position
  normalize: mobile                                                    // optional LUFS target
}
```

| Setting | Type | Description |
|---------|------|-------------|
| `codec` | identifier | Output audio codec |
| `channels` | identifier or number | Channel layout (`stereo`, `5.1`, etc.) or count |
| `source` | `prefer(filter)` | Filter expression selecting preferred source track |
| `normalize` | preset/block | Optional EBU R128 loudness normalization |
| `bitrate` | string | Target bitrate (e.g., `"192k"`, `"320k"`) |
| `skip_if_exists` | `{ filter }` | Don't create if a matching track already exists |
| `create_if` | condition | Only create when this condition is true |
| `title` | string | Title for the new track |
| `language` | identifier or `inherit` | Language code, or `inherit` from source |
| `position` | identifier or number | Where to insert: `after_source`, `last`, or index |

## Conditional Logic

### `when` / `else`

Execute actions conditionally.

```
when <condition> {
  <actions>
}

when <condition> {
  <actions>
} else {
  <actions>
}
```

### `rules` Block

Named rules with a match mode.

```
rules first {        // stop after first matching rule
  rule "rule-name" {
    when <condition> {
      <actions>
    }
  }
}

rules all {          // evaluate all rules
  rule "rule-name" {
    when <condition> {
      <actions>
    }
  }
}
```

## Conditions

Conditions are boolean expressions used in `when`, `skip when`, and `rules` blocks.

### Logical Operators

```
<condition> and <condition>      // both must be true
<condition> or <condition>       // either can be true
not <condition>                  // negation
(<condition>)                    // grouping
```

Precedence (highest to lowest): `not`, `and`, `or`. Use parentheses to override.

### Track Existence

```
exists(audio where lang == jpn)
exists(subtitle where forced)
exists(audio where codec in [truehd, dts_hd])
```

Track queries use the same targets as track operations — `audio`, `subtitle`/`subtitles`, `video`, `attachments` — plus the `track` wildcard which matches any track type.

### Track Count

```
count(audio) > 1
count(subtitle where lang == eng) == 0
count(audio where channels >= 6) >= 1
```

### Built-in Predicates

| Predicate | Description |
|-----------|-------------|
| `audio_is_multi_language` | File has audio tracks in multiple languages |
| `is_dubbed` | File appears to be dubbed (heuristic) |
| `is_original` | File appears to be the original language version |

### Field Comparison

Access nested fields using dot notation:

```
video.codec == hevc
video.codec in [hevc, h265]
audio.channels >= 6
```

### Field Existence

Check if a field (especially plugin metadata) exists:

```
plugin.radarr.original_language exists
plugin.sonarr.series_id exists
```

### Comparison Operators

| Operator | Description |
|----------|-------------|
| `==` | Equal |
| `!=` | Not equal |
| `<` | Less than |
| `>` | Greater than |
| `<=` | Less than or equal |
| `>=` | Greater than or equal |
| `in` | Contained in list |

## Filter Expressions

Filters are used in `where` clauses to match tracks. They share the logical operators with conditions but have track-specific predicates.

### Language Filter

```
lang in [eng, jpn, und]          // language is in list
lang == eng                       // exact language match
lang == plugin.radarr.original_language   // compare to a dynamic field
lang != plugin.sonarr.original_language   // not-equal with field reference
```

The right-hand side of `lang ==` and `lang !=` can be a **field reference** (dot-separated path) instead of a literal value. At evaluation time, the field is resolved against the media file's plugin metadata. If the field does not exist, the comparison evaluates to `false`.

### Codec Filter

```
codec in [aac, ac3, eac3]        // codec is in list
codec == truehd                   // exact codec match
codec == plugin.detector.codec    // compare to a dynamic field
```

Like language filters, `codec ==` and `codec !=` also accept field references on the right-hand side.

### Channel Filter

```
channels >= 6                     // 5.1 or higher
channels == 2                     // stereo
```

### Flag Filters

| Filter | Description |
|--------|-------------|
| `commentary` | Track is classified as commentary |
| `forced` | Track has the forced flag |
| `default` | Track has the default flag |
| `font` | Attachment is a font file |

### Title Filter

```
title contains "commentary"       // title includes substring
title matches "Director.*"        // title matches pattern
```

### Combining Filters

```
lang in [eng] and not commentary
codec in [truehd, dts_hd] and channels >= 6
(lang == jpn or lang == und) and not forced
```

## Actions

Actions are executed inside `when` and `rules` blocks.

| Action | Syntax | Description |
|--------|--------|-------------|
| Skip | `skip` or `skip <phase>` | Skip current or named phase |
| Warn | `warn "<message>"` | Log a warning |
| Fail | `fail "<message>"` | Fail processing with an error |
| Set Default | `set_default <track_ref>` | Set default flag on matching tracks |
| Set Forced | `set_forced <track_ref>` | Set forced flag on matching tracks |
| Set Language | `set_language <track_ref> <value>` | Set language on matching tracks |
| Set Tag | `set_tag "<key>" <value>` | Set a container-level tag |
| Delete Tag | `delete_tag "<key>"` | Remove a container-level tag |
| Clear Tags | `clear_tags` | Remove all container-level tags |

### Track References

Actions that target tracks use a track reference:

```
set_default audio where lang == eng
set_forced subtitle where lang == eng
set_language audio where default plugin.radarr.original_language
```

### String Interpolation

Warning and failure messages support `{filename}` interpolation:

```
warn "No English subtitles in {filename}"
fail "Unsupported container for {filename}"
```

## Primitives

### Values

| Type | Examples | Description |
|------|----------|-------------|
| String | `"hello"`, `"192k"` | Double-quoted string |
| Number | `20`, `1080`, `5.1`, `192k` | Integer, float, or suffixed number |
| Boolean | `true`, `false` | Boolean value (word-boundary aware, won't match `truehd`) |
| Identifier | `hevc`, `eng`, `medium` | Unquoted name |
| List | `[eng, jpn, und]` | Comma-separated values in brackets (trailing comma allowed) |

### Language Codes

Use ISO 639-2/B three-letter language codes:

| Code | Language |
|------|----------|
| `eng` | English |
| `jpn` | Japanese |
| `und` | Undetermined |
| `chi` | Chinese |
| `kor` | Korean |
| `fre` | French |
| `ger` | German |
| `spa` | Spanish |
| `zxx` | No linguistic content (music/effects) |
| `mul` | Multiple languages |

### Codec Names

Codec names are normalized by the compiler (e.g., `h265` → `hevc`). Common codecs:

| Video | Audio | Subtitle |
|-------|-------|----------|
| `hevc` / `h265` | `aac` | `srt` |
| `h264` / `avc` | `ac3` | `ass` / `ssa` |
| `av1` | `eac3` | `pgs` / `hdmv_pgs` |
| `vp9` | `truehd` | `vobsub` / `dvd_subtitle` |
| `mpeg2` | `dts` / `dts_hd` | `subrip` |
| | `flac` | |
| | `opus` | |
| | `pcm` | |

### Limits

| Limit | Value |
|-------|-------|
| Maximum policy source size | 1 MiB (1,048,576 bytes) |

## Compilation Pipeline

```
Source (.voom)
    │
    ▼
  Parser (pest → AST) ── syntax and structural errors
    │
    ▼
  Validator ────── semantic errors:
    │               • Unknown codec (with "did you mean?" suggestions)
    │               • Unknown container format
    │               • Unknown language code
    │               • Invalid `on_error` value (config and phase level)
    │               • Circular phase dependencies
    │               • Duplicate phase names
    │               • Unreachable phases (referenced in `depends_on` but not defined)
    │               • Unknown phase in `run_if`
    │               • Invalid `run_if` trigger
    │               • Conflicting keep/remove on the same track type
    │               • `set_tag`/`delete_tag` conflict on the same tag key
    │               • `set_tag` before `clear_tags` ordering error
    │               • Invalid number suffix
    │               • Invalid defaults strategy
    ▼
  Compiler ─────── CompiledPolicy (domain types, ready for evaluation)
```

### Programmatic API

```rust
use voom_dsl::{parse_policy, validate, compile, compile_ast, format_policy};

// Parse source to AST
let ast = parse_policy(source)?;

// Validate semantics (returns Result<(), ValidationErrors>)
validate(&ast)?;

// Compile to domain types (parse + validate + compile in one step)
let compiled = compile(source)?;
// or compile from an existing AST
let compiled = compile_ast(&ast)?;

// Format/pretty-print (takes &PolicyAst, returns String)
let formatted = format_policy(&ast);
```

## Complete Example

```
policy "production-normalize" {
  config {
    languages audio: [eng, und]
    languages subtitle: [eng, und]
    commentary_patterns: ["commentary", "director", "cast"]
    on_error: continue
  }

  // Phase 1: Ensure MKV container
  phase containerize {
    container mkv
  }

  // Phase 2: Normalize tracks
  phase normalize {
    depends_on: [containerize]

    audio actions {
      clear_all_default: true
      clear_all_forced: true
      clear_all_titles: true
    }

    subtitle actions {
      clear_all_default: true
      clear_all_forced: true
    }

    keep audio where lang in [eng, jpn, und]
    keep subtitles where lang in [eng] and not commentary
    remove attachments where not font

    order tracks [
      video, audio_main, audio_alternate,
      subtitle_main, subtitle_forced,
      audio_commentary, subtitle_commentary, attachment
    ]

    defaults {
      audio: first_per_language
      subtitle: none
    }
  }

  // Phase 3: Transcode if needed
  phase transcode {
    skip when video.codec in [hevc, h265]

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
    }
  }

  // Phase 4: Create compatibility audio
  phase audio_compat {
    depends_on: [normalize]

    synthesize "Stereo AAC" {
      codec: aac
      channels: stereo
      source: prefer(codec in [truehd, dts_hd, flac] and channels >= 6)
      bitrate: "192k"
      skip_if_exists { codec in [aac] and channels == 2 and not commentary }
      title: "Stereo (AAC)"
      language: inherit
      position: after_source
    }
  }

  // Phase 5: Validation rules
  phase validate {
    depends_on: [transcode, audio_compat]
    run_if transcode.modified

    when exists(audio where lang == jpn) and not exists(subtitle where lang == eng) {
      warn "Japanese audio but no English subtitles in {filename}"
    }

    rules first {
      rule "multi-language" {
        when audio_is_multi_language {
          warn "Multiple audio languages in {filename}"
        }
      }
    }
  }

  // Phase 6: Plugin metadata
  phase metadata {
    when plugin.radarr.original_language exists {
      set_language audio where default plugin.radarr.original_language
      set_tag "title" plugin.radarr.title
    }
  }
}
```
