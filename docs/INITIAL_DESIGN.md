# Video Orchestration Operations Manager (VOOM): Full Rust Re-Implementation with WASM Plugin Architecture

## 1. Context & Motivation

1. **Plugin-first architecture with WASM** — The core is a thin kernel. ALL functionality (scanning, introspection, transcoding, metadata enrichment, storage) is implemented as plugins. Core plugins are native Rust for performance; third-party plugins are WASM modules loaded via wasmtime, enabling language-agnostic plugin authorship (Rust, Go, C, Zig, etc.) with sandboxed execution.

2. **Custom block-based DSL** — Replaces YAML policy configuration with a purpose-built language using curly-brace blocks and media-specific keywords.

**Design philosophy:** Quality over speed. Single compiled binary with embedded core plugins. WASM sandbox for third-party extensions. Strong type safety throughout.

**Clean break:** No backward compatibility with VPO v1 databases, configs, or policies.

### Tech stack

| Component | Technology | Crate |
|-----------|-----------|-------|
| Language | Rust (2024 edition) | — |
| CLI | clap (derive) | `clap` |
| Web server | axum | `axum`, `tower`, `tokio` |
| Web frontend | htmx + Alpine.js | — (static assets) |
| Templates | tera | `tera` |
| Database | SQLite | `rusqlite` |
| Config | TOML | `toml`, `serde` |
| DSL parser | pest | `pest`, `pest_derive` |
| WASM runtime | wasmtime | `wasmtime`, `wit-bindgen` |
| Async runtime | tokio | `tokio` |
| Serialization | serde + MessagePack | `serde`, `rmp-serde` |
| Hashing | xxHash | `xxhash-rust` |
| HTTP client | reqwest | `reqwest` |
| Logging | tracing | `tracing`, `tracing-subscriber` |
| Testing | built-in + insta | `insta` (snapshot tests) |
| Error handling | thiserror + anyhow | `thiserror`, `anyhow` |
| File walking | walkdir + rayon | `walkdir`, `rayon`, `ignore` |
| UUID | uuid | `uuid` |
| DateTime | chrono | `chrono` |

---

## 2. Architecture Overview

### 2.1 Layer diagram

```
┌────────────────────────────────────────────────────────────────┐
│                     Presentation Layer                         │
│   ┌─────────────────────┐    ┌──────────────────────────────┐  │
│   │    CLI (clap)       │    │  Web UI (axum + htmx)        │  │
│   └─────────────────────┘    └──────────────────────────────┘  │
├────────────────────────────────────────────────────────────────┤
│                       Core Kernel                              │
│   ┌────────────┐  ┌───────────┐  ┌────────────────────────┐    │
│   │  Event Bus │  │ Registry  │  │  Plugin Loader         │    │
│   │(sync/prio) │  │           │  │  (native + wasmtime)   │    │
│   └────────────┘  └───────────┘  └────────────────────────┘    │
├────────────────────────────────────────────────────────────────┤
│                      DSL Engine                                │
│   ┌────────┐ ┌────────┐ ┌──────────┐ ┌──────────┐ ┌───────┐    │
│   │ Lexer  │ │ Parser │ │ Compiler │ │Validator │ │Printer│    │
│   │ (pest) │ │ (pest) │ │          │ │          │ │       │    │
│   └────────┘ └────────┘ └──────────┘ └──────────┘ └───────┘    │
├────────────────────────────────────────────────────────────────┤
│            Native Plugins (compiled into binary)               │
│                                                                │
│   Discovery ────── Introspection ────── Storage                │
│   Evaluator ────── Orchestrator ─────── Jobs                   │
│   MKVToolNix ───── FFmpeg ──────────── Backup                  │
│   Web Server ───── Tool Detector                               │
│                                                                │
├────────────────────────────────────────────────────────────────┤
│            WASM Plugins (loaded at runtime via wasmtime)       │
│                                                                │
│   Radarr ───────── Sonarr ──────────── Whisper                 │
│   TVDB ─────────── HandBrake ─────────── Custom...             │
│                                                                │
├────────────────────────────────────────────────────────────────┤
│                   Domain Types (shared)                        │
│   MediaFile · Track · Plan · Action · Event · Capability       │
│   (serde-serializable, shared via WIT interface for WASM)      │
└────────────────────────────────────────────────────────────────┘
```

### 2.2 Two-tier plugin model

**Native plugins** (compiled into the binary):
- Zero overhead — direct function calls via trait objects
- Full access to Rust ecosystem
- Used for performance-critical core functionality
- Enabled/disabled via Cargo feature flags

**WASM plugins** (loaded at runtime):
- Sandboxed execution — cannot access filesystem or network directly
- Language-agnostic — write in any language that compiles to WASM
- Communicate with host via WIT (WebAssembly Interface Types)
- Host provides capability functions (read files, invoke tools, store data)
- Slight serialization overhead at the boundary

### 2.3 Key architectural principles

1. **Kernel is inert** — Zero media knowledge. Manages plugin lifecycle, event dispatch, and capability routing.
2. **Capabilities, not types** — Plugins declare capabilities. The kernel routes work based on capability matching.
3. **Plan as contract** — Evaluator produces `Plan` structs. Executors consume them. Plans are serializable and inspectable.
4. **Events for coordination** — Plugins communicate through the event bus. No plugin directly calls another.
5. **Domain types as lingua franca** — All plugins share the same types from the `domain` crate. For WASM plugins, these are exposed via WIT.
6. **Immutable data** — Domain types are `Clone` but not `mut`. Mutations produce new values.

### 2.4 Data flow

```
DSL Policy File (.voom)              Media Files on Disk
      │                                     │
      ▼                                     ▼
  ┌────────┐                        ┌──────────────┐
  │  pest   │                        │  Discovery   │ (rayon + walkdir)
  │ parser  │                        │  Plugin      │
  │compiler │                        └──────┬───────┘
  └────┬────┘                               │
       │                          FileDiscovered events
       ▼                                    │
  CompiledPolicy                     ┌──────▼───────┐
       │                             │ Introspector  │ (ffprobe)
       │                             │ Plugin        │
       │                             └──────┬───────┘
       │                          FileIntrospected events
       │                                    │
       │                             ┌──────▼───────┐
       │                             │   Storage     │ (rusqlite)
       │                             │   Plugin      │
       │                             └──────┬───────┘
       │                                    │
       ▼                                    ▼
  ┌─────────────────────────────────────────────┐
  │           Phase Orchestrator Plugin          │
  └──────────────────┬──────────────────────────┘
                     │
  ┌──────────────────▼──────────────────────────┐
  │         Policy Evaluator Plugin             │
  │  (classifies tracks, evaluates conditions,  │
  │   produces Plan)                            │
  └──────────────────┬──────────────────────────┘
                     │ PlanCreated event
                     ▼
  ┌─────────────────────────────────────────────┐
  │  Executor Plugin (MKVToolNix or FFmpeg)      │
  │  (selected by capability match)             │
  └──────────────────┬──────────────────────────┘
                     │ PlanCompleted event
                     ▼
              Modified media file
```

---

## 3. Workspace Structure

```
voom/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── voom-kernel/              # Core kernel: event bus, registry, loader
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── bus.rs            # Event bus (synchronous priority-ordered dispatch, parking_lot::RwLock)
│   │       ├── registry.rs       # Plugin registry
│   │       ├── loader.rs         # Native + WASM plugin loading
│   │       ├── capabilities.rs   # Capability descriptors
│   │       └── manifest.rs       # Plugin manifest
│   │
│   ├── voom-domain/              # Shared domain types
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── media.rs          # MediaFile, Track, TrackType
│   │       ├── plan.rs           # Plan, PlannedAction, ActionResult
│   │       ├── events.rs         # All event types
│   │       ├── errors.rs         # Error types
│   │       └── capabilities.rs   # Capability type definitions
│   │
│   ├── voom-dsl/                 # DSL parser and compiler
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── grammar.pest      # PEG grammar
│   │       ├── ast.rs            # AST node types
│   │       ├── parser.rs         # pest → AST
│   │       ├── compiler.rs       # AST → CompiledPolicy
│   │       ├── validator.rs      # Semantic validation
│   │       ├── formatter.rs      # Pretty-printer
│   │       └── errors.rs         # Parse/compile errors with spans
│   │
│   ├── voom-cli/                 # CLI binary
│   │   └── src/
│   │       ├── main.rs
│   │       ├── commands/
│   │       │   ├── scan.rs
│   │       │   ├── inspect.rs
│   │       │   ├── process.rs
│   │       │   ├── policy.rs
│   │       │   ├── plugin.rs
│   │       │   ├── serve.rs
│   │       │   ├── doctor.rs
│   │       │   ├── jobs.rs
│   │       │   ├── report.rs
│   │       │   ├── db.rs
│   │       │   └── config.rs
│   │       └── output.rs         # Formatting, tables, progress
│   │
│   ├── voom-wit/                 # WIT interface definitions for WASM plugins
│   │   └── wit/
│   │       ├── plugin.wit        # Plugin interface
│   │       ├── host.wit          # Host functions exposed to plugins
│   │       └── types.wit         # Shared type definitions
│   │
│   ├── voom-plugin-sdk/          # SDK crate for plugin authors
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── host.rs           # Host function bindings
│   │       └── macros.rs         # Proc macros for plugin boilerplate
│   │
│   └── plugins/                  # Native plugins (compiled into binary)
│       ├── discovery/
│       ├── ffprobe-introspector/
│       ├── tool-detector/
│       ├── sqlite-store/
│       ├── policy-evaluator/
│       ├── phase-orchestrator/
│       ├── mkvtoolnix-executor/
│       ├── ffmpeg-executor/
│       ├── backup-manager/
│       ├── job-manager/
│       └── web-server/
│
├── wasm-plugins/                 # WASM plugins (compiled separately)
│   ├── radarr-metadata/
│   ├── sonarr-metadata/
│   ├── whisper-transcriber/
│   ├── audio-synthesizer/
│   └── tvdb-metadata/
│
├── web/                          # Static web assets
│   ├── static/
│   │   ├── css/
│   │   └── js/
│   └── templates/                # Tera templates
│
└── tests/
    ├── fixtures/                 # Test media files, policies
    └── integration/
```

---

## 4. Core Kernel Design

### 4.1 Plugin trait

```rust
/// Universal plugin interface. All native plugins implement this.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    fn handles(&self, event_type: &str) -> bool;
    fn on_event(&self, event: &Event) -> Result<Option<EventResult>>;

    /// Optional lifecycle hooks
    fn init(&mut self, _ctx: &PluginContext) -> Result<()> { Ok(()) }
    fn shutdown(&self) -> Result<()> { Ok(()) }
}
```

### 4.2 Capability system

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Capability {
    Discover { schemes: Vec<String> },
    Introspect { formats: Vec<String> },
    Evaluate,
    Execute {
        operations: Vec<String>,   // "metadata", "reorder", "remux", "transcode"
        formats: Vec<String>,       // empty = all
    },
    Store { backend: String },
    DetectTools,
    ManageJobs,
    ServeHttp,
    Orchestrate,
    Backup,
    EnrichMetadata { source: String },
    Transcribe,
    Synthesize,
}
```

### 4.3 Event bus

```rust
pub struct EventBus {
    subscribers: RwLock<HashMap<String, Vec<Subscriber>>>,
}

struct Subscriber {
    plugin_name: String,
    priority: i32,          // Lower = runs first
    handler: Arc<dyn Plugin>,
}

impl EventBus {
    /// Dispatch event to all subscribers, ordered by priority.
    /// Synchronous — uses `parking_lot::RwLock` for dispatch.
    pub fn publish(&self, event: Event) -> Vec<EventResult> { ... }

    /// Subscribe a plugin to an event type.
    pub fn subscribe(&self, event_type: &str, plugin: Arc<dyn Plugin>, priority: i32) { ... }
}
```

### 4.4 WASM plugin loading

```rust
pub struct WasmPluginLoader {
    engine: wasmtime::Engine,
    linker: wasmtime::Linker<HostState>,
}

/// State exposed to WASM plugins via host functions.
struct HostState {
    registry: Arc<PluginRegistry>,
    storage: Arc<dyn StorageTrait>,
    tool_runner: Arc<ToolRunner>,
}

impl WasmPluginLoader {
    /// Load a .wasm plugin file, validate it, and wrap it as a Plugin trait object.
    pub fn load(&self, path: &Path) -> Result<Arc<dyn Plugin>> { ... }
}
```

### 4.5 WIT interface (plugin ↔ host contract)

```wit
// wit/plugin.wit — What plugins must implement

interface plugin {
    record plugin-info {
        name: string,
        version: string,
        capabilities: list<capability>,
    }

    variant capability {
        discover(discover-cap),
        introspect(introspect-cap),
        evaluate,
        execute(execute-cap),
        enrich-metadata(enrich-cap),
        transcribe,
        synthesize,
    }

    // Plugin entry points
    get-info: func() -> plugin-info;
    handles: func(event-type: string) -> bool;
    on-event: func(event: event-data) -> option<event-result>;
}

// wit/host.wit — What the host provides to plugins

interface host {
    // File operations (sandboxed to media library paths)
    read-file-metadata: func(path: string) -> result<media-file, error>;
    list-files: func(filters: file-filters) -> result<list<media-file>, error>;

    // Tool invocation (plugins request, host executes)
    run-tool: func(tool: string, args: list<string>, timeout-ms: u64) -> result<tool-output, error>;

    // Storage
    get-plugin-data: func(key: string) -> option<list<u8>>;
    set-plugin-data: func(key: string, value: list<u8>) -> result<_, error>;

    // HTTP (for metadata plugins that need API access)
    http-get: func(url: string, headers: list<header>) -> result<http-response, error>;
    http-post: func(url: string, headers: list<header>, body: list<u8>) -> result<http-response, error>;

    // Logging
    log: func(level: log-level, message: string);
}

// wit/types.wit — Shared type definitions
interface types {
    record media-file {
        path: string,
        size: u64,
        content-hash: string,
        container: string,
        duration: float64,
        bitrate: option<u32>,
        tracks: list<track>,
        tags: list<key-value>,
        plugin-metadata: list<plugin-metadata-entry>,
    }

    record track {
        index: u32,
        track-type: track-type,
        codec: string,
        language: string,
        title: string,
        is-default: bool,
        is-forced: bool,
        channels: option<u32>,
        width: option<u32>,
        height: option<u32>,
        frame-rate: option<float64>,
        is-vfr: bool,
        is-hdr: bool,
    }

    enum track-type {
        video,
        audio-main,
        audio-alternate,
        audio-commentary,
        audio-music,
        audio-sfx,
        audio-non-speech,
        subtitle-main,
        subtitle-forced,
        subtitle-commentary,
        attachment,
    }

    // ... (Plan, Action, etc.)
}
```

### 4.6 Plugin context and lifecycle

```rust
pub struct PluginContext {
    pub config: PluginConfig,       // Plugin-specific TOML config
    pub data_dir: PathBuf,          // ~/.config/voom/plugins/<name>/
    pub logger: tracing::Span,      // Scoped logger
}

// Lifecycle:
// 1. Loader discovers plugin (native or WASM)
// 2. Loader reads manifest, validates version compatibility
// 3. Plugin::init(ctx) called with config and data directory
// 4. Plugin registered in registry, subscribed to events
// 5. ... runtime ...
// 6. Plugin::shutdown() called on application exit
```

---

## 5. Domain Model

### 5.1 Media types (`voom-domain/src/media.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: Uuid,
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: String,
    pub container: Container,
    pub duration: f64,              // seconds
    pub bitrate: Option<u32>,       // bits/sec
    pub tracks: Vec<Track>,
    pub tags: HashMap<String, String>,
    pub plugin_metadata: HashMap<String, serde_json::Value>,
    pub introspected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub index: u32,
    pub track_type: TrackType,
    pub codec: String,
    pub language: String,           // ISO 639-2/B
    pub title: String,
    pub is_default: bool,
    pub is_forced: bool,
    // Audio
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    // Video
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub is_vfr: bool,
    pub is_hdr: bool,
    pub hdr_format: Option<String>,
    pub pixel_format: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackType {
    Video,
    AudioMain,
    AudioAlternate,
    AudioCommentary,
    AudioMusic,
    AudioSfx,
    AudioNonSpeech,
    SubtitleMain,
    SubtitleForced,
    SubtitleCommentary,
    Attachment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Container {
    Mkv, Mp4, Avi, Webm, Flv, Wmv, Mov, Ts, Other,
}
```

### 5.2 Plan types (`voom-domain/src/plan.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub file: MediaFile,
    pub policy_name: String,
    pub phase_name: String,
    pub actions: Vec<PlannedAction>,
    pub warnings: Vec<String>,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub operation: OperationType,
    pub track_index: Option<u32>,
    pub parameters: serde_json::Value,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    SetDefault, ClearDefault,
    SetForced, ClearForced,
    SetTitle, SetLanguage,
    RemoveTrack, ReorderTracks,
    ConvertContainer,
    TranscodeVideo, TranscodeAudio,
    SynthesizeAudio,
    SetContainerTag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub phase_name: String,
    pub outcome: PhaseOutcome,
    pub actions: Vec<ActionResult>,
    pub file_modified: bool,
    pub skip_reason: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseOutcome {
    Pending, Completed, Skipped, Failed,
}
```

### 5.3 Event types (`voom-domain/src/events.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    FileDiscovered(FileDiscoveredEvent),
    FileIntrospected(FileIntrospectedEvent),
    MetadataEnriched(MetadataEnrichedEvent),
    PolicyEvaluate(PolicyEvaluateEvent),
    PlanCreated(PlanCreatedEvent),
    PlanExecuting(PlanExecutingEvent),
    PlanCompleted(PlanCompletedEvent),
    PlanFailed(PlanFailedEvent),
    JobStarted(JobStartedEvent),
    JobProgress(JobProgressEvent),
    JobCompleted(JobCompletedEvent),
    ToolDetected(ToolDetectedEvent),
}

impl Event {
    pub fn event_type(&self) -> &str {
        match self {
            Event::FileDiscovered(_) => "file.discovered",
            Event::FileIntrospected(_) => "file.introspected",
            Event::PlanCreated(_) => "plan.created",
            // ...
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiscoveredEvent {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIntrospectedEvent {
    pub file: MediaFile,
}

// ... other event structs
```

---

## 6. DSL Design

### 6.1 PEG grammar (`voom-dsl/src/grammar.pest`)

```pest
WHITESPACE = _{ " " | "\t" | NEWLINE }
COMMENT    = _{ "//" ~ (!NEWLINE ~ ANY)* }

policy = { SOI ~ "policy" ~ string ~ "{" ~ config? ~ phase+ ~ "}" ~ EOI }

config = { "config" ~ "{" ~ config_item* ~ "}" }
config_item = {
    "languages" ~ ("audio" | "subtitle") ~ ":" ~ list
  | "on_error" ~ ":" ~ ident
  | "commentary_patterns" ~ ":" ~ list
}

phase = { "phase" ~ ident ~ "{" ~ phase_item* ~ "}" }
phase_item = {
    skip_when | depends_on | run_if | on_error
  | container_op | keep_op | remove_op | order_op
  | defaults_op | actions_op | transcode_op | synthesize_op
  | when_block | rules_block
}

// Phase control
skip_when  = { "skip" ~ "when" ~ condition }
depends_on = { "depends_on" ~ ":" ~ list }
run_if     = { "run_if" ~ ident ~ "." ~ ("modified" | "completed") }
on_error   = { "on_error" ~ ":" ~ ident }

// Operations
container_op = { "container" ~ ident }

keep_op   = { "keep" ~ track_target ~ where_clause? }
remove_op = { "remove" ~ track_target ~ where_clause? }
track_target = { "audio" | "subtitles" | "attachments" }
where_clause = { "where" ~ filter_expr }

order_op   = { "order" ~ "tracks" ~ list }
defaults_op = { "defaults" ~ "{" ~ default_item* ~ "}" }
default_item = { ("audio" | "subtitle") ~ ":" ~ ident }

actions_op = { ("audio" | "subtitle" | "video") ~ "actions" ~ "{" ~ action_setting* ~ "}" }
action_setting = { ident ~ ":" ~ value }

transcode_op = { "transcode" ~ ("video" | "audio") ~ "to" ~ ident ~ block? }
block = { "{" ~ kv_pair* ~ "}" }
kv_pair = { ident ~ ":" ~ value }

synthesize_op = { "synthesize" ~ string ~ "{" ~ synth_item* ~ "}" }
synth_item = {
    "codec" ~ ":" ~ ident
  | "channels" ~ ":" ~ (ident | number)
  | "source" ~ ":" ~ source_pref
  | "bitrate" ~ ":" ~ string
  | "skip_if_exists" ~ "{" ~ filter_expr ~ "}"
  | "create_if" ~ condition
  | "title" ~ ":" ~ string
  | "language" ~ ":" ~ (ident | "inherit")
  | "position" ~ ":" ~ (ident | number)
}
source_pref = { "prefer" ~ "(" ~ filter_expr ~ ")" }

// Conditions (used in when blocks and skip_when)
when_block = { "when" ~ condition ~ "{" ~ action* ~ "}" ~ else_block? }
else_block = { "else" ~ "{" ~ action* ~ "}" }

rules_block = { "rules" ~ match_mode ~ "{" ~ rule_item* ~ "}" }
match_mode  = { "first" | "all" }
rule_item   = { "rule" ~ string ~ "{" ~ when_block ~ "}" }

condition = { condition_or }
condition_or  = { condition_and ~ ("or" ~ condition_and)* }
condition_and = { condition_not ~ ("and" ~ condition_not)* }
condition_not = { "not" ~ condition_atom | condition_atom }
condition_atom = {
    "exists" ~ "(" ~ track_query ~ ")"
  | "count" ~ "(" ~ track_query ~ ")" ~ compare_op ~ number
  | "audio_is_multi_language"
  | "is_dubbed"
  | "is_original"
  | field_access ~ compare_op ~ value
  | field_access ~ "exists"
  | "(" ~ condition ~ ")"
}

field_access = { ident ~ ("." ~ ident)+ }
track_query  = { (track_target | "track") ~ ("where" ~ filter_expr)? }

// Filter expressions (used in where clauses)
filter_expr = { filter_or }
filter_or   = { filter_and ~ ("or" ~ filter_and)* }
filter_and  = { filter_not ~ ("and" ~ filter_not)* }
filter_not  = { "not" ~ filter_atom | filter_atom }
filter_atom = {
    "lang" ~ "in" ~ list
  | "codec" ~ "in" ~ list
  | "channels" ~ compare_op ~ number
  | "commentary"
  | "forced"
  | "default"
  | "font"
  | "title" ~ ("contains" | "matches") ~ string
  | "(" ~ filter_expr ~ ")"
}

// Actions
action = {
    "skip" ~ ident?
  | "warn" ~ string
  | "fail" ~ string
  | "set_default" ~ track_ref
  | "set_forced" ~ track_ref
  | "set_language" ~ track_ref ~ (string | field_access)
  | "set_tag" ~ string ~ value
}
track_ref = { track_target ~ ("where" ~ filter_expr)? }

// Primitives
compare_op = { "==" | "!=" | "<=" | ">=" | "<" | ">" | "in" }
list       = { "[" ~ (value ~ ("," ~ value)*)? ~ ","? ~ "]" }
value      = { string | number | ident | list | "true" | "false" }
string     = @{ "\"" ~ (!"\"" ~ ANY)* ~ "\"" }
number     = @{ ASCII_DIGIT+ ~ ("." ~ ASCII_DIGIT+)? ~ ASCII_ALPHA* }
ident      = @{ (ASCII_ALPHA | "_") ~ (ASCII_ALPHANUMERIC | "_")* }
```

### 6.2 Complete DSL example

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
      bitrate: 192k
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

### 6.3 AST types (`voom-dsl/src/ast.rs`)

```rust
#[derive(Debug, Clone)]
pub struct PolicyAst {
    pub name: String,
    pub config: Option<ConfigNode>,
    pub phases: Vec<PhaseNode>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ConfigNode {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub on_error: Option<String>,
    pub commentary_patterns: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PhaseNode {
    pub name: String,
    pub skip_when: Option<ConditionNode>,
    pub depends_on: Vec<String>,
    pub run_if: Option<RunIfNode>,
    pub on_error: Option<String>,
    pub operations: Vec<OperationNode>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum OperationNode {
    Container(String),
    Keep { target: String, filter: Option<FilterNode> },
    Remove { target: String, filter: Option<FilterNode> },
    Order(Vec<String>),
    Defaults(Vec<(String, String)>),
    Actions { target: String, settings: Vec<(String, Value)> },
    Transcode { target: String, codec: String, settings: Vec<(String, Value)> },
    Synthesize { name: String, settings: Vec<SynthSetting> },
    When(WhenNode),
    Rules { mode: String, rules: Vec<RuleNode> },
}

#[derive(Debug, Clone)]
pub struct WhenNode {
    pub condition: ConditionNode,
    pub then_actions: Vec<ActionNode>,
    pub else_actions: Vec<ActionNode>,
}

#[derive(Debug, Clone)]
pub enum ConditionNode {
    Exists(TrackQueryNode),
    Count(TrackQueryNode, CompareOp, f64),
    FieldCompare(Vec<String>, CompareOp, Value),
    FieldExists(Vec<String>),
    AudioIsMultiLanguage,
    IsDubbed,
    IsOriginal,
    And(Vec<ConditionNode>),
    Or(Vec<ConditionNode>),
    Not(Box<ConditionNode>),
}

#[derive(Debug, Clone)]
pub enum FilterNode {
    LangIn(Vec<String>),
    CodecIn(Vec<String>),
    Channels(CompareOp, f64),
    Commentary,
    Forced,
    Default,
    Font,
    TitleContains(String),
    TitleMatches(String),
    And(Vec<FilterNode>),
    Or(Vec<FilterNode>),
    Not(Box<FilterNode>),
}

#[derive(Debug, Clone)]
pub enum ActionNode {
    Skip(Option<String>),
    Warn(String),
    Fail(String),
    SetDefault(TrackRefNode),
    SetForced(TrackRefNode),
    SetLanguage(TrackRefNode, ValueOrField),
    SetTag(String, ValueOrField),
}

#[derive(Debug, Clone)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub col: usize,
}
```

### 6.4 Compilation pipeline

```
Source text (.voom file)
    │
    ▼
┌──────────┐     pest::Error with span
│   pest    │ ──────────────────────────►  ParseError { span, message, suggestion }
│  parser   │
└─────┬─────┘
      │ pest Pairs (CST)
      ▼
┌──────────┐     BuildError with span
│ AST build │ ──────────────────────────►  ParseError
│           │
└─────┬─────┘
      │ PolicyAst
      ▼
┌──────────┐     ValidationError with span
│ Validator │ ──────────────────────────►  - Unknown codec "h256" (did you mean "h265"?)
│           │                              - Circular dependency: a → b → a
│           │                              - Phase "foo" is unreachable
│           │                              - Conflicting actions on track 0
└─────┬─────┘
      │ Validated PolicyAst
      ▼
┌──────────┐
│ Compiler  │ ──►  CompiledPolicy
│           │       (domain types, ready for evaluation)
└──────────┘
```

---

## 7. Plugin Specifications

### 7.1 Native plugins (compiled into binary)

| Plugin | Crate | Capability | External Dep |
|--------|-------|-----------|--------------|
| `discovery` | `plugins/discovery` | `Discover { schemes: ["file"] }` | None (walkdir + rayon) |
| `ffprobe-introspector` | `plugins/ffprobe-introspector` | `Introspect { formats: [...] }` | ffprobe |
| `tool-detector` | `plugins/tool-detector` | `DetectTools` | PATH lookup |
| `sqlite-store` | `plugins/sqlite-store` | `Store { backend: "sqlite" }` | None (rusqlite) |
| `policy-evaluator` | `plugins/policy-evaluator` | `Evaluate` | None (pure logic) |
| `phase-orchestrator` | `plugins/phase-orchestrator` | `Orchestrate` | None (pure logic) |
| `mkvtoolnix-executor` | `plugins/mkvtoolnix-executor` | `Execute { ops: [metadata, reorder, filter, remux] }` | mkvpropedit, mkvmerge |
| `ffmpeg-executor` | `plugins/ffmpeg-executor` | `Execute { ops: [metadata, remux, transcode] }` | ffmpeg |
| `backup-manager` | `plugins/backup-manager` | `Backup` | None (fs ops) |
| `job-manager` | `plugins/job-manager` | `ManageJobs` | None (tokio tasks) |
| `web-server` | `plugins/web-server` | `ServeHttp` | None (axum) |

### 7.2 WASM plugins (loaded at runtime)

| Plugin | Capability | External Dep | Language |
|--------|-----------|--------------|----------|
| `radarr-metadata` | `EnrichMetadata { source: "radarr" }` | Radarr API (via host HTTP) | Rust→WASM |
| `sonarr-metadata` | `EnrichMetadata { source: "sonarr" }` | Sonarr API (via host HTTP) | Rust→WASM |
| `whisper-transcriber` | `Transcribe` | Whisper (via host tool runner) | Rust→WASM |
| `audio-synthesizer` | `Synthesize` | ffmpeg (via host tool runner) | Rust→WASM |
| `tvdb-metadata` | `EnrichMetadata { source: "tvdb" }` | TVDB API (via host HTTP) | Python→WASM |

### 7.3 Example future community plugins

| Plugin | Capability | Notes |
|--------|-----------|-------|
| `handbrake-executor` | `Execute { ops: [transcode] }` | HandBrakeCLI wrapper |
| `plex-metadata` | `EnrichMetadata { source: "plex" }` | Plex API |
| `jellyfin-metadata` | `EnrichMetadata { source: "jellyfin" }` | Jellyfin API |
| `s3-backup` | `Backup` | AWS S3 backup target |
| `subtitle-extract` | `Execute { ops: [extract] }` | Extract subtitles to SRT |
| `postgres-store` | `Store { backend: "postgres" }` | Alternative storage |

### 7.4 Executor routing

```rust
fn select_executor(
    plan: &Plan,
    registry: &PluginRegistry,
) -> Result<Arc<dyn Plugin>> {
    let operations: HashSet<&str> = plan.actions.iter()
        .map(|a| a.operation.as_str())
        .collect();
    let container = &plan.file.container;

    let candidates = registry.find_by_capability_type::<Execute>();
    let compatible: Vec<_> = candidates.iter()
        .filter(|p| {
            let cap = p.get_capability::<Execute>().unwrap();
            operations.iter().all(|op| cap.operations.contains(&op.to_string()))
                && (cap.formats.is_empty() || cap.formats.contains(&container.to_string()))
        })
        .collect();

    compatible.into_iter()
        .min_by_key(|p| p.priority())
        .cloned()
        .ok_or_else(|| anyhow!("No executor for {:?} on {}", operations, container))
}
```

---

## 8. Storage Design

### 8.1 Schema (`plugins/sqlite-store`)

```sql
CREATE TABLE files (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    container TEXT NOT NULL,
    duration REAL,
    bitrate INTEGER,
    tags TEXT,                     -- JSON
    plugin_metadata TEXT,          -- JSON
    introspected_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    stream_index INTEGER NOT NULL,
    track_type TEXT NOT NULL,
    codec TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'und',
    title TEXT NOT NULL DEFAULT '',
    is_default INTEGER NOT NULL DEFAULT 0,
    is_forced INTEGER NOT NULL DEFAULT 0,
    channels INTEGER,
    channel_layout TEXT,
    sample_rate INTEGER,
    bit_depth INTEGER,
    width INTEGER,
    height INTEGER,
    frame_rate REAL,
    is_vfr INTEGER NOT NULL DEFAULT 0,
    is_hdr INTEGER NOT NULL DEFAULT 0,
    hdr_format TEXT,
    pixel_format TEXT,
    UNIQUE(file_id, stream_index)
);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 100,
    payload TEXT,
    progress REAL DEFAULT 0.0,
    progress_message TEXT,
    output TEXT,
    error TEXT,
    worker_id TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT
);

CREATE TABLE plans (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id),
    policy_name TEXT NOT NULL,
    phase_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    actions TEXT NOT NULL,         -- JSON
    warnings TEXT,
    created_at TEXT NOT NULL,
    executed_at TEXT,
    result TEXT
);

CREATE TABLE processing_stats (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id),
    policy_name TEXT NOT NULL,
    phase_name TEXT NOT NULL,
    outcome TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    actions_taken INTEGER NOT NULL,
    tracks_modified INTEGER NOT NULL,
    file_size_before INTEGER,
    file_size_after INTEGER,
    created_at TEXT NOT NULL
);

CREATE TABLE plugin_data (
    plugin_name TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (plugin_name, key)
);

-- Indexes
CREATE INDEX idx_files_path ON files(path);
CREATE INDEX idx_files_hash ON files(content_hash);
CREATE INDEX idx_tracks_file ON tracks(file_id);
CREATE INDEX idx_jobs_status ON jobs(status, priority);
CREATE INDEX idx_plans_file ON plans(file_id);
CREATE INDEX idx_stats_file ON processing_stats(file_id);
```

### 8.2 Storage trait

```rust
/// Synchronous — backed by blocking rusqlite with r2d2 connection pool.
/// Web handlers use `tokio::task::spawn_blocking` to call these methods.
pub trait StorageTrait: Send + Sync {
    // Files
    fn upsert_file(&self, file: &MediaFile) -> Result<()>;
    fn get_file(&self, id: &Uuid) -> Result<Option<MediaFile>>;
    fn get_file_by_path(&self, path: &Path) -> Result<Option<MediaFile>>;
    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>>;
    fn delete_file(&self, id: &Uuid) -> Result<()>;

    // Jobs
    fn create_job(&self, job: &Job) -> Result<Uuid>;
    fn get_job(&self, id: &Uuid) -> Result<Option<Job>>;
    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()>;
    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>>;

    // Plans
    fn save_plan(&self, plan: &Plan) -> Result<Uuid>;

    // Stats
    fn record_stats(&self, stats: &ProcessingStats) -> Result<()>;

    // Plugin data
    fn get_plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()>;

    // Maintenance
    fn vacuum(&self) -> Result<()>;
    fn prune_missing_files(&self) -> Result<u64>;
}
```

---

## 9. CLI Design

### 9.1 Command tree (clap derive)

```
voom
├── scan <path>                    # Discover and introspect files
│   ├── --recursive / -r
│   ├── --workers <n>
│   └── --no-hash                  # Skip content hashing
├── inspect <file>                 # Show file metadata
│   ├── --format json|table
│   └── --tracks-only
├── process <path>                 # Apply policy to files
│   ├── --policy <file.voom>
│   ├── --dry-run
│   ├── --on-error skip|continue|fail
│   ├── --workers <n>
│   ├── --approve                  # Require approval per file
│   └── --no-backup
├── policy
│   ├── list
│   ├── validate <file.voom>
│   ├── show <file.voom>
│   └── format <file.voom>         # Auto-format in place
├── plugin
│   ├── list
│   ├── info <name>
│   ├── enable <name>
│   ├── disable <name>
│   └── install <path.wasm>       # Install WASM plugin
├── jobs
│   ├── list [--status <s>]
│   ├── status <id>
│   └── cancel <id>
├── report [--format json|table]
├── doctor                         # System health check
├── serve
│   ├── --port <n>
│   └── --host <addr>
├── db
│   ├── prune
│   ├── vacuum
│   └── reset
├── config
│   ├── show
│   └── edit
├── init                           # First-time setup
├── status                         # Daemon/library status
└── completions <shell>            # Generate shell completions
```

### 9.2 Policy file extension

`.voom` — e.g., `normalize.voom`, `production-pipeline.voom`

---

## 10. Web UI Design

### 10.1 Technology

- **Server**: axum with tower middleware
- **Reactivity**: htmx for server-driven partial updates, Alpine.js for client state
- **Templates**: Tera (Jinja2-like syntax)
- **Styling**: Custom CSS (no framework)
- **Live updates**: SSE via axum
- **No build step**: Static assets served directly

### 10.2 Routes

| Route | Description |
|-------|-------------|
| `/` | Dashboard: library stats, recent activity, health |
| `/library` | File browser with search/filter/sort |
| `/files/:id` | File detail: tracks, metadata, history |
| `/policies` | Policy list with status |
| `/policies/:name/edit` | DSL editor with highlighting + validation |
| `/jobs` | Job monitor with progress |
| `/plugins` | Plugin manager: list, enable/disable, info |
| `/stats` | Processing statistics and charts |
| `/settings` | Configuration, tool status |
| `/api/*` | JSON REST API (mirrors CLI functionality) |
| `/events` | SSE endpoint for live updates |

### 10.3 DSL editor

- Custom tokenizer in JS (mirrors `grammar.pest` token types)
- Syntax highlighting with CSS classes per token type
- Live validation via `/api/policy/validate` (debounced POST)
- Error markers with line/column from `Span`
- Auto-complete for keywords, codecs, languages
- Format button (POST to `/api/policy/format`, replace editor content)
- Preview: dry-run against a selected file

---

## 11. Sprint Plan

### Sprint 1: Core Kernel & Plugin Protocol (Size: L)

**Goal:** Build the kernel: event bus, plugin trait, registry, native + WASM loader.

**Deliverables:**
- `voom-kernel` crate: `bus.rs`, `registry.rs`, `loader.rs`, `capabilities.rs`, `manifest.rs`
- `voom-domain` crate (minimal): `events.rs` (enough for bus testing)
- `voom-wit`: WIT interface definitions for WASM plugins
- Native plugin loading via trait objects
- WASM plugin loading via wasmtime + WIT bindings
- Test: load a native plugin and a WASM plugin, dispatch events, query capabilities

**Depends on:** Nothing
**Exit criteria:** Dispatch an event to both a native and WASM plugin. Capability queries work.

---

### Sprint 2: Domain Model & Core Utilities (Size: M)

**Goal:** Complete the shared type system.

**Deliverables:**
- `voom-domain` crate (complete): `media.rs`, `plan.rs`, `events.rs`, `errors.rs`, `capabilities.rs`
- Core utilities: codec mappings, language code validation, string normalization, datetime helpers
- Serde serialization for all types (JSON + MessagePack for WASM boundary)
- WIT type definitions in `voom-wit` matching domain types

**Depends on:** Sprint 1
**Exit criteria:** All types constructable, serializable, and tested. WIT types match Rust types.

---

### Sprint 3: DSL Lexer & Parser (Size: XL)

**Goal:** Implement the PEG grammar and AST builder.

**Deliverables:**
- `voom-dsl` crate: `grammar.pest`, `ast.rs`, `parser.rs`, `errors.rs`
- Complete PEG grammar covering all constructs from §6.1
- pest CST → typed AST conversion
- Error messages with source spans and suggestions (e.g., "Unknown keyword 'transcode_video'. Did you mean 'transcode video'?")
- Snapshot tests (insta) for all grammar constructs

**Depends on:** Sprint 2
**Exit criteria:** Parse the complete example policy from §6.2 into a valid AST.

---

### Sprint 4: DSL Compiler & Validation (Size: XL)

**Goal:** AST → CompiledPolicy with semantic validation.

**Deliverables:**
- `voom-dsl`: `compiler.rs`, `validator.rs`, `formatter.rs`
- Semantic validation: unknown codecs (with did-you-mean), circular phase deps, unreachable phases, conflicting actions, invalid language codes
- Name resolution for phase references, codec aliases
- Pretty-printer (AST → formatted source)
- Round-trip tests: source → parse → format → parse → compare ASTs

**Depends on:** Sprint 3, Sprint 2
**Exit criteria:** DSL text → validated CompiledPolicy. Errors include source spans. Round-trip formatting works.

---

### Sprint 5: Storage Plugin (Size: L)

**Goal:** SQLite persistence as a native plugin.

**Deliverables:**
- `plugins/sqlite-store`: schema (§8.1), connection pool, typed queries
- `StorageTrait` in `voom-domain` (§8.2)
- WAL mode, foreign keys, busy timeout
- Connection pool for concurrent access (r2d2 or deadpool)

**Depends on:** Sprint 1, Sprint 2
**Parallelizable with:** Sprints 3–4
**Exit criteria:** CRUD for files, tracks, jobs, plans. Pool tested under concurrent load.

---

### Sprint 6: Discovery & Introspection Plugins (Size: L)

**Goal:** File scanning and metadata extraction as native plugins.

**Deliverables:**
- `plugins/discovery`: parallel walk (rayon + walkdir), xxHash64 content hashing, `FileDiscovered` events
- `plugins/ffprobe-introspector`: ffprobe JSON → `MediaFile` + `Track`, codec detection, HDR/VFR detection, `FileIntrospected` events
- `plugins/tool-detector`: PATH lookup, version parsing, capability caching (serde + fs cache)

**Depends on:** Sprint 5, Sprint 1
**Exit criteria:** Scan directory → discover → introspect → store. End-to-end test with real media.

---

### Sprint 7: Policy Evaluation Plugin (Size: XL)

**Goal:** Evaluate compiled policies against files to produce Plans.

**Deliverables:**
- `plugins/policy-evaluator`:
  - Track classification (main vs alternate vs commentary vs music vs sfx)
  - Condition evaluation (EXISTS, COUNT, AND/OR/NOT, multi-language, plugin metadata, field access)
  - Action application (skip, warn, fail, set_default, set_forced, set_language, set_tag)
  - Track filtering (keep/remove with where clauses)
  - Conditional rules (first-match-wins, evaluate-all)
  - Emits `PlanCreated` events
- `plugins/phase-orchestrator`:
  - Phase sequencing with skip_when, depends_on, run_if
  - Re-introspection between phases (detects file modifications)
  - Per-phase on_error handling (skip/continue/fail)

**Depends on:** Sprint 4 (compiled policies), Sprint 6 (introspected files)
**Exit criteria:** Policy + file → Plan. Dry-run output is human-readable. All condition/action types tested.

---

### Sprint 8: Executor Plugins (Size: XL)

**Goal:** Media modification via native executor plugins.

**Deliverables:**
- `plugins/mkvtoolnix-executor`:
  - mkvpropedit: in-place MKV metadata (flags, titles, language)
  - mkvmerge: track reorder, filter, container → MKV
- `plugins/ffmpeg-executor`:
  - Non-MKV metadata via ffmpeg
  - Container conversion (any → MP4)
  - Video transcoding (hevc, h264, vp9, av1) — CRF, bitrate, constrained quality
  - Audio transcoding (aac, eac3, ac3, opus, flac)
  - Hardware acceleration (nvenc, qsv, vaapi, videotoolbox) with CPU fallback
  - FFmpeg command builder, stderr progress parsing
  - Skip conditions, VFR detection, bitrate estimation, HDR warnings
- `plugins/backup-manager`: file backup, disk space validation, restore
- Capability-based executor routing (§7.4)

**Depends on:** Sprint 7 (Plans), Sprint 6 (tool detector)
**Exit criteria:** Scan → evaluate → execute. Test with real MKV and MP4 files.

---

### Sprint 9: CLI Layer (Size: L)

**Goal:** Complete clap-based CLI.

**Deliverables:**
- `voom-cli` crate with all commands from §9.1
- Kernel bootstrap: load config, init plugins, wire events
- Output formatting: tables (comfy-table), progress bars (indicatif), colored output (owo-colors)
- Shell completions (clap_complete)
- Error display with context and suggestions

**Depends on:** Sprints 5–8
**Exit criteria:** All commands functional. Integration tests via assert_cmd.

---

### Sprint 10: Jobs & Background Processing (Size: M)

**Goal:** Background job system using tokio tasks.

**Deliverables:**
- `plugins/job-manager`:
  - Job queue with priority and status tracking
  - tokio::spawn for concurrent workers (configurable concurrency)
  - Job lifecycle: claim, progress, complete, fail, cancel
  - Stats collection per-phase
- Progress reporting:
  - Stderr (CLI mode) via indicatif
  - Database (daemon mode)
  - FFmpeg stderr progress parsing
- Batch processing with `--workers` and `--on-error`

**Depends on:** Sprint 8, Sprint 5
**Exit criteria:** Process 10+ files concurrently with live progress. Error recovery works.

---

### Sprint 11: Web UI & REST API (Size: XL)

**Goal:** Daemon mode with web dashboard.

**Deliverables:**
- `plugins/web-server`:
  - axum application with tower middleware
  - REST API (JSON): files, jobs, plans, plugins, stats, policy validate/format
  - Tera templates with htmx + Alpine.js
  - SSE for live scan/job progress
  - Auth (token-based), CSRF, rate limiting, CSP headers
- DSL policy editor (§10.3)
- All pages from §10.2
- `voom serve` command

**Depends on:** Sprint 9, Sprint 10
**Exit criteria:** Web dashboard loads, DSL editor works, job progress streams via SSE.

---

### Sprint 12: WASM Plugins & SDK (Size: L)

**Goal:** Ship WASM plugins, plugin SDK, and documentation.

**Deliverables:**
- `wasm-plugins/radarr-metadata`: Movie metadata enrichment (uses host HTTP functions)
- `wasm-plugins/sonarr-metadata`: TV metadata enrichment
- `wasm-plugins/whisper-transcriber`: Transcription (uses host tool runner)
- `wasm-plugins/audio-synthesizer`: Audio synthesis (uses host tool runner)
- `wasm-plugins/tvdb-metadata`: TV metadata enrichment from TVDB (uses host HTTP functions)
- `wasm-plugins/handbrake-executor`: Video transcoding (similar to ffmpeg)
- `voom-plugin-sdk` crate:
  - Host function bindings
  - Proc macros for plugin boilerplate (`#[voom_plugin]`, `#[on_event(...)]`)
  - Example plugin template
- Documentation:
  - DSL language reference
  - Plugin development guide (native + WASM)
  - CLI reference
  - Architecture overview

**Depends on:** All previous sprints
**Exit criteria:** Feature parity with VPO v1. WASM plugin SDK usable for third-party development.

---

## 12. Summary Table

| Sprint | Name | Size | Depends | Key Risk |
|--------|------|------|---------|----------|
| 1 | Core Kernel & Plugin Protocol | L | — | WASM/WIT interface design complexity |
| 2 | Domain Model & Core Utilities | M | S1 | Missing edge-case types discovered later |
| 3 | DSL Lexer & Parser | XL | S2 | Grammar expressiveness vs. simplicity |
| 4 | DSL Compiler & Validation | XL | S3 | Covering all policy semantics in DSL |
| 5 | Storage Plugin (SQLite) | L | S1, S2 | rusqlite blocking + connection pooling |
| 6 | Discovery & Introspection | L | S5 | ffprobe JSON edge cases |
| 7 | Policy Evaluation | XL | S4, S6 | Condition/action combinatorics |
| 8 | Executor Plugins | XL | S7 | FFmpeg transcoding complexity, HW accel |
| 9 | CLI Layer | L | S5–S8 | Integration testing breadth |
| 10 | Jobs & Background Processing | M | S8, S5 | Tokio task management, progress fidelity |
| 11 | Web UI & REST API | XL | S9, S10 | DSL editor UX in browser |
| 12 | WASM Plugins & SDK | L | All | WASM serialization overhead, SDK ergonomics |

**Critical path:** S1 → S2 → S3 → S4 → S7 → S8 → S9
**Parallelizable:** S5 with S3–S4; S6 after S5, overlapping S4; S10+S11 overlap

---

## 13. Verification Strategy

| Sprint | Test Approach |
|--------|---------------|
| S1 | Unit: event dispatch, plugin loading (native + WASM), capability queries |
| S2 | Unit: type construction, serde round-trip, WIT type mapping |
| S3 | Snapshot (insta): parse every grammar construct. Error message tests. |
| S4 | Snapshot: compile fixtures. Validation error tests. Format round-trip. |
| S5 | Integration: CRUD, pool concurrency (loom or tokio test) |
| S6 | Integration: real ffprobe against fixture media files |
| S7 | Unit + integration: policy evaluation with fixture files + expected Plans |
| S8 | Integration: execute Plans against real media (requires ffmpeg/mkvtoolnix) |
| S9 | Integration: assert_cmd smoke tests for all CLI commands |
| S10 | Integration: concurrent job processing, progress accuracy |
| S11 | Integration: axum test client for API, tower-test for middleware |
| S12 | Unit: WASM plugin loading + event handling. SDK example tests. |

**End-to-end acceptance (after S9):**
1. `voom scan /path` → files appear in `voom inspect`
2. Write `.voom` policy → `voom policy validate` passes
3. `voom process --dry-run` shows expected Plan
4. `voom process` modifies files correctly
5. `voom report` shows statistics

---

## 14. Risk Analysis

| Risk | Impact | Mitigation |
|------|--------|------------|
| WASM/WIT interface stability | High | Pin wasmtime version, keep WIT interface small and stable |
| WASM serialization overhead for hot paths | Medium | Core plugins are native (zero overhead). Only metadata plugins use WASM. |
| pest grammar limitations for complex syntax | Medium | pest handles PEG well; fallback to hand-written parser if needed |
| rusqlite blocking calls from async context | Low | Use spawn_blocking for DB calls from web handlers |
| FFmpeg command building complexity | High | Port tested logic from v1, comprehensive integration tests |
| Compilation time for large workspace | Medium | Separate crates, feature flags, cargo-nextest for parallel tests |
| Plugin SDK ergonomics in Rust | Medium | Proc macros to reduce boilerplate, comprehensive examples |
| htmx + Alpine.js limitations for DSL editor | Medium | May need a small standalone JS editor component (CodeMirror 6) |

---

## 15. Glossary

| Term | Definition |
|------|-----------|
| **Kernel** | Core framework: event bus, plugin loader, registry. Zero media knowledge. |
| **Native plugin** | Plugin compiled directly into the voom binary as a Rust crate. Zero-overhead trait object dispatch. |
| **WASM plugin** | Plugin compiled to WebAssembly, loaded at runtime via wasmtime. Sandboxed, language-agnostic. |
| **WIT** | WebAssembly Interface Types — the contract between WASM plugins and the host kernel. |
| **Capability** | Enum variant declaring what a plugin can do (e.g., `Execute { ops: [transcode] }`). |
| **DSL** | Domain-Specific Language — the `.voom` file format for policy definitions. |
| **AST** | Abstract Syntax Tree — typed representation produced by parsing DSL source. |
| **CompiledPolicy** | Rust structs produced by compiling a validated AST. Ready for evaluation. |
| **Plan** | Execution plan: list of actions to perform on a file. Produced by evaluator, consumed by executors. |
| **Phase** | Named stage in a policy pipeline. Phases execute sequentially with optional skip/dependency conditions. |
| **Event** | Typed enum variant published to the event bus. Plugins subscribe by event type string. |

---

## 16. Addendum — Plugin contract rev-6 (#378)

The original design (sections 1–15 above) described two plugin tiers:
kernel-registered plugins invoked via `Plugin::on_event` and library-only
crates the CLI called directly. The rev-6 contract collapses this
distinction. All former library-only crates (`discovery`,
`policy-evaluator`, `phase-orchestrator`) are now kernel-registered and
invoked via `Kernel::dispatch_to_capability`. The kernel exposes three
communication primitives — event broadcast, unary Call, streaming Call —
and times every invocation through the shared stats sink.

The full design rationale, including `Call` / `CallResponse` shape,
capability resolution discipline, and the WASM `on-call` ABI, lives in
`docs/superpowers/specs/2026-05-14-plugin-stats-self-reporting-design.md`.
See `docs/architecture.md` ("Communication primitives — Events vs Calls"
and "Capability-based routing") for the runtime view.
