# voom-dsl fuzz harnesses

Fuzz targets for the VOOM DSL parser and compile pipeline, driven by `cargo-fuzz` and `libFuzzer`.

## Targets

| Target | Entry point | What it exercises |
|--------|-------------|-------------------|
| `parse_policy` | `voom_dsl::parse_policy` | PEG grammar, lexer, AST construction |
| `compile_policy` | `voom_dsl::compile_policy` | Parser + validator + AST→domain compiler |

## Running locally

Requires nightly Rust and `cargo-fuzz`:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

Run a target for 60 seconds:

```bash
cd crates/voom-dsl/fuzz
# Copy committed seeds into the working corpus (libFuzzer mutates this dir)
mkdir -p corpus/parse_policy
cp seeds/parse_policy/*.voom corpus/parse_policy/

cargo +nightly fuzz run parse_policy -- -max_total_time=60
```

Same pattern for `compile_policy`. Replace `60` with whatever wall time you can spare.

### Expected throughput

On a modern x86_64 box you should see roughly:

| Target | Approximate throughput |
|--------|-----------------------|
| `parse_policy` | ~200k+ execs/sec |
| `compile_policy` | ~80k+ execs/sec |

`compile_policy` is slower because it runs the full parse → validate → compile pipeline including an xxh3 hash on every input. If you see throughput an order of magnitude lower than this, suspect a regression — open an issue.

## When the fuzzer finds a crash

`libFuzzer` writes the offending input to `artifacts/<target>/crash-<sha256>`. Reproduce with:

```bash
cargo +nightly fuzz run parse_policy artifacts/parse_policy/crash-<sha>
```

1. File a GitHub issue, attach the crash input (it's small).
2. Once the bug is fixed, copy the crash input into `seeds/<target>/` so it becomes a permanent regression seed.

## Corpus hygiene

- `seeds/<target>/` — committed canonical inputs. Hand-curated. Never deleted.
- `corpus/<target>/` — libFuzzer's working directory. Gitignored, mutated in place. Recreate from `seeds/` whenever you start a fresh run.
- `artifacts/<target>/` — crash reproducers. Gitignored. Save interesting ones into `seeds/` after a fix lands.

## Why is this excluded from the workspace?

The fuzzer requires nightly Rust. Keeping `crates/voom-dsl/fuzz/` out of the main workspace (`Cargo.toml` `exclude` list) lets stable toolchain builds (`cargo build --workspace`) ignore it entirely.

`Cargo.lock` IS committed in this fuzz crate (despite being a sub-package) so that crash reproductions remain bit-reproducible across machines and time — the standard cargo-fuzz convention.
