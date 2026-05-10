# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

VOOM (Video Orchestration Operations Manager) is a policy-driven video library manager being built in Rust. It is a from-scratch rewrite of VPO (Video Policy Orchestrator) with a WASM plugin architecture and a custom block-based DSL for policy configuration.

**Status:** Active development (Sprints 1–12 complete, Sprint 13 next). All core crates, 12 native plugins (kernel-registered) + 3 library-only plugins, CLI, web UI, and WASM plugin SDK are implemented. ~800+ tests. See `docs/INITIAL_DESIGN.md` for the original design and `docs/architecture.md` for current architecture.

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

Plugins communicate exclusively through an **event bus** (synchronous priority-ordered dispatch with `parking_lot::RwLock`). No plugin directly calls another. Executor selection currently uses event-bus priority ordering; capability-based routing is planned for a future sprint.

### Workspace crates (`crates/`)
- **voom-kernel** — Event bus, plugin registry, native + WASM loader
- **voom-domain** — Shared types: `MediaFile`, `Track`, `Plan`, `Event`, `Capability` (serde-serializable, exposed to WASM via WIT)
- **voom-dsl** — PEG grammar (pest), parser, AST, compiler (AST → CompiledPolicy), validator, formatter
- **voom-cli** — clap-derive CLI binary (20 subcommands: scan, inspect, process, policy, plugin, jobs, report, files, plans, events, health, doctor, serve, db, config, tools, history, backup, init, completions)
- **voom-process** — Shared subprocess utilities with timeout-aware execution for executor plugins
- **voom-wit** — WIT interface definitions (plugin.wit, host.wit, types.wit)
- **voom-plugin-sdk** — SDK crate for third-party plugin authors
- **plugins/** — 12 kernel-registered (bus-tracer, capability-collector, health-checker, tool-detector, discovery, ffprobe-introspector, mkvtoolnix-executor, ffmpeg-executor, backup-manager, sqlite-store, job-manager, report) + 4 library/command-started (policy-evaluator: called directly by CLI, phase-orchestrator: called directly by CLI, web-server: started by `serve`, web-sse-bridge: registered when `serve` runs).

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

Rust 2024 edition. Key crates: clap (CLI), axum/tokio (web + async), pest (DSL parser), wasmtime/wit-bindgen (WASM plugins), rusqlite (SQLite), serde/rmp-serde (serialization), tracing (logging), insta (snapshot tests), thiserror/anyhow (errors), walkdir/rayon (file walking), xxhash-rust (hashing).

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

## Review Process

When review agents surface pre-existing issues that are out of scope for the current branch, or a plan chooses to defer issues that might be out-of-scope, create a GitHub issue for each rather than fixing them in-place. This keeps branches focused and ensures deferred work is tracked.

## Pre-Commit Checks
- After implementing changes, always run `cargo test` and `cargo clippy` before committing. If tests fail, fix them before proceeding.
- Run `cargo fmt` before every commit. Never commit without formatting first.

## Pre-PR Checks
- Before submitting a PR, the full test suite **including functional tests** must pass:
  ```bash
  cargo test                                                         # unit + integration tests
  cargo test -p voom-cli --features functional -- --test-threads=4   # functional (end-to-end) tests
  ```
- Do not submit a PR if any test fails. Fix all failures first.

## Long-Running Commands
- The `Bash` tool's hard cap is 10 minutes. The functional test suite (`cargo test -p voom-cli --features functional`) regularly exceeds this. **Always** dispatch it (and any other potentially long-running command — full workspace `cargo test`, large `cargo build` from cold cache, etc.) with `run_in_background: true`. Do not raise the foreground `timeout` parameter and hope — the cap is fixed.
- After dispatching a background command, **wait for the auto-completion notification**. Do not poll the output file, do not re-run the command, do not sleep. The system delivers the result when it's ready.
- Read the output file only after the notification arrives.

## Git Workflow
- When staging commits, be precise about which files to include. Use `git add <specific-files>` rather than `git add .` to avoid staging unrelated changes.

## Handling Test Errors
There is no such thing as a pre-existing test error. If you encoutner a test error while making changes then **YOU** are responsible for fixing it, no matter the original source or the software module that displays the error. This work **CANNOT** be deferred to a Github issue.

## Planning
- When asked to create a plan, produce a written deliverable quickly. Limit exploration to 5 minutes max before starting to write. Do not over-explore the codebase.

## Project Structure
- This is a Rust workspace with WASM plugins. Always check that changes compile with `cargo build` and that WASM plugins build separately if modified.

## Configuration

- App config: TOML at `~/.config/voom/config.toml`
- Plugin data: `~/.config/voom/plugins/<name>/`
- WASM plugins directory: `~/.config/voom/plugins/wasm/`

