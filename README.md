# Video Orchestration Operations Manager (VOOM)

A policy-driven video library manager built in Rust. VOOM automatically normalizes, organizes, and processes video files according to declarative policies written in a custom DSL.

## Architecture

VOOM uses a two-tier plugin model around a thin kernel with zero media knowledge:

- **Native plugins** (compiled into the binary) handle discovery, introspection (ffprobe), policy evaluation, execution (FFmpeg/MKVToolNix), storage (SQLite), backup, job management, and a web UI.
- **WASM plugins** (loaded at runtime via wasmtime) extend functionality in a sandboxed, language-agnostic way.

All inter-plugin communication flows through a synchronous, priority-ordered **event bus**. The kernel routes work via **capability matching**, not hardcoded types.

See [`docs/INITIAL_DESIGN.md`](docs/INITIAL_DESIGN.md) for the full design document and [`CLAUDE.md`](CLAUDE.md) for development conventions.

## Quick Start

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests (~800+)
cargo clippy --workspace       # Lint
cargo fmt --all                # Format
cargo run -- --help            # CLI usage
```

## Policy DSL

Policies use `.voom` files with a custom block-based syntax:

```
policy "normalize" {
  phase "audio" {
    keep audio where codec in ["aac", "eac3", "truehd"]
    remove audio where codec in ["mp3"]
    defaults audio language "eng"
  }
}
```

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `voom-cli` | CLI binary with 14 subcommands |
| `voom-kernel` | Event bus, plugin registry, capability routing |
| `voom-domain` | Shared types: `MediaFile`, `Track`, `Plan`, `Event` |
| `voom-dsl` | PEG parser, AST, compiler, validator, formatter |
| `voom-wit` | WIT interface definitions for WASM plugins |
| `voom-plugin-sdk` | SDK for third-party WASM plugin authors |

## License

See [LICENSE](LICENSE) for details.
