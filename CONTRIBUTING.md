# Contributing to VOOM

## Coverage

CI generates an LCOV report on every push to `main` and every pull request and uploads it to SonarCloud. To reproduce the same report locally, install [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) once:

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked
```

Then run one of the following:

```bash
# Open an interactive HTML report in the browser.
cargo llvm-cov --workspace --html --open

# Generate the same lcov.info CI uploads to SonarCloud.
cargo llvm-cov --workspace --lcov --output-path lcov.info

# Coverage for a single crate (faster turnaround while iterating).
cargo llvm-cov -p voom-dsl --html --open
```

`cargo llvm-cov --workspace` covers the root cargo workspace under `crates/` and `plugins/`. The `wasm-plugins/` directory is a separate cargo workspace and is not currently included in the coverage baseline; if you need WASM plugin coverage, run `cargo llvm-cov` from inside `wasm-plugins/` against that workspace.

The CI invocation lives in `.github/workflows/sonarcloud.yml`. The path `lcov.info` is referenced by `sonar-project.properties` via `sonar.rust.lcov.reportPaths`; do not rename it without updating both files.

## Fuzzing the DSL

The `voom-dsl` parser and compiler have fuzz harnesses under `crates/voom-dsl/fuzz/`. See `crates/voom-dsl/fuzz/README.md` for instructions on running locally and triaging crashes. The harnesses also run weekly in CI via `.github/workflows/fuzz.yml`.

## Mutation testing

The `voom-dsl`, `voom-policy-evaluator`, and `voom-phase-orchestrator` crates run nightly mutation testing in CI via `.github/workflows/mutants.yml`. To reproduce a single-crate run locally:

```bash
cargo install cargo-mutants --locked
cargo mutants -p voom-dsl
```

Surviving mutants and per-mutant outcomes land in `mutants.out/`. The workspace config at `.cargo/mutants.toml` excludes tests and benches and the workflow caps each cargo command at 300 seconds via `--timeout 300` — pass `--timeout <seconds>` to override locally.

The current baseline counts per crate are recorded in `docs/mutation-testing-baseline.md`; new code in the targeted crates should aim to keep the missed-mutant count flat or drive it down.
