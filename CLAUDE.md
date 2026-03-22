# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

VOOM (Video Orchestration Operations Manager) is a policy-driven video library manager being built in Rust. It is a from-scratch rewrite of VPO (Video Policy Orchestrator) with a WASM plugin architecture and a custom block-based DSL for policy configuration.

**Status:** Active development (Sprints 1–12 complete, Sprint 13 next). All core crates, 11 native plugins, CLI, web UI, and WASM plugin SDK are implemented. ~800+ tests. See `docs/INITIAL_DESIGN.md` for the original design and `docs/architecture.md` for current architecture.

## Build & Development Commands

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests
cargo test -p voom-dsl         # Run tests for a single crate
cargo run -- <subcommand>      # Run the CLI (voom-cli)
cargo clippy --workspace       # Lint
cargo fmt --all                # Format
```

## Architecture

### Two-tier plugin model
The core is a thin kernel with zero media knowledge. ALL functionality is implemented as plugins:
- **Native plugins** — compiled into the binary as trait objects, zero overhead
- **WASM plugins** — loaded at runtime via wasmtime, sandboxed, language-agnostic (via WIT interfaces)

Plugins communicate exclusively through an **event bus** (synchronous priority-ordered dispatch with `parking_lot::RwLock`). No plugin directly calls another. The kernel routes work based on **capability matching**, not hardcoded types.

### Workspace crates (`crates/`)
- **voom-kernel** — Event bus, plugin registry, native + WASM loader, capability routing
- **voom-domain** — Shared types: `MediaFile`, `Track`, `Plan`, `Event`, `Capability` (serde-serializable, exposed to WASM via WIT)
- **voom-dsl** — PEG grammar (pest), parser, AST, compiler (AST → CompiledPolicy), validator, formatter
- **voom-cli** — clap-derive CLI binary with subcommands (scan, inspect, process, policy, plugin, serve, doctor, jobs, report, db, config)
- **voom-wit** — WIT interface definitions (plugin.wit, host.wit, types.wit)
- **voom-plugin-sdk** — SDK crate for third-party plugin authors
- **plugins/** — Native plugins: discovery, ffprobe-introspector, tool-detector, sqlite-store, policy-evaluator, phase-orchestrator, mkvtoolnix-executor, ffmpeg-executor, backup-manager, job-manager, web-server

### Key data flow
1. DSL policy file (`.voom`) → pest parser → AST → CompiledPolicy
2. Discovery plugin walks filesystem → `FileDiscovered` events
3. Introspector plugin (ffprobe) → `FileIntrospected` events → Storage plugin (SQLite)
4. Phase Orchestrator feeds files + policy to Policy Evaluator → `Plan` structs
5. Executor plugin (MKVToolNix or FFmpeg, selected by capability) executes the Plan

### Key design principles
- **Plan as contract** — Evaluator produces serializable/inspectable `Plan` structs; executors consume them
- **Immutable domain types** — `Clone` but mutations produce new values
- **Events for coordination** — all inter-plugin communication via the event bus
- **Domain types as lingua franca** — shared via `voom-domain`, exposed to WASM via WIT

## Tech Stack

Rust 2021 edition. Key crates: clap (CLI), axum/tokio (web + async), pest (DSL parser), wasmtime/wit-bindgen (WASM plugins), rusqlite (SQLite), serde/rmp-serde (serialization), tracing (logging), insta (snapshot tests), thiserror/anyhow (errors), walkdir/rayon (file walking), xxhash-rust (hashing).

Web frontend: htmx + Alpine.js with Tera templates.

## DSL

Policy files use `.voom` extension and a custom curly-brace block syntax (not YAML). See `docs/INITIAL_DESIGN.md` section 6 for the full PEG grammar and examples. Key constructs: `policy`, `phase` (with `depends_on`, `skip when`, `run_if`), track operations (`keep`, `remove`, `order`, `defaults`), `transcode`, `synthesize`, `when`/`else` conditionals, `rules` blocks.

## Code Conventions

### Progress bar filename truncation

Any progress line that includes a filename **must** use `shrink_filename()` and `max_filename_len()` from `crate::output` to prevent terminal line wrapping. Compute the fixed-width overhead by measuring the actual non-filename content of the line, not by guessing a constant. The pattern is:

```rust
use crate::output::{max_filename_len, shrink_filename};

// Build the prefix/surrounding text first, then measure it
let prefix = format!("Discovering... {count} files found — ");
let max_name = max_filename_len(2 + prefix.len()); // 2 = spinner + space
let name = shrink_filename(&raw_filename, max_name);
pb.set_message(format!("{prefix}{name}"));
```

For indicatif templates with bars/counters (where the overhead is rendered by indicatif, not your format string), estimate the template overhead and pass it to `max_filename_len()`.

## Configuration

- App config: TOML at `~/.config/voom/config.toml`
- Plugin data: `~/.config/voom/plugins/<name>/`
- WASM plugins directory: `~/.config/voom/plugins/wasm/`
