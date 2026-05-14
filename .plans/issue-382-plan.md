# Plan: voom estimate should use open_store, not bootstrap_kernel (#382)

## Problem

`crates/voom-cli/src/commands/estimate.rs:50` calls
`bootstrap_kernel_with_store` and uses only `result.store`, ignoring
`result.kernel` and `result.collector`. Same anti-pattern that was fixed
for `voom plugin stats` in commit `de69c75`. Side effects:

- Full plugin initialization (bus-tracer, health-checker, tool-detector,
  sqlite-store) runs for a read-only-ish operation.
- Bus-tracer writes `event_log` rows for every init event.
- Plugin-stats sink (#383) records timing rows for those init dispatches.

## Verification that `estimate calibrate` only needs the store

`calibrate()` body (estimate.rs:48-64):

- Loads config.
- Inserts cost-model samples via `store.insert_cost_model_sample(...)`.
- Prints a count.

No kernel dispatch, no event bus use, no capability collector. The fix
is mechanical.

## Changes

### `crates/voom-cli/src/commands/estimate.rs`

```rust
async fn calibrate(args: &EstimateArgs) -> Result<()> {
    let config = crate::config::load_config()?;
    let store = crate::app::open_store(&config)?;            // ← was bootstrap_kernel_with_store
    let completed_at = chrono::Utc::now();
    // ... unchanged ...
}
```

### `crates/voom-cli/tests/plugin_stats_e2e.rs`

Add a regression test mirroring
`plugin_stats_query_does_not_mutate_database`: assert that running
`voom estimate calibrate` does NOT add rows to `plugin_stats` or
`event_log`.

## Edge cases / risks

- `estimate.rs` has a separate non-calibrate path that goes through
  `into_process_args` + `commands::process::run`. That path is
  unchanged (it needs the kernel).
- No public-API changes; pure internal substitution.

## Test plan

```bash
cargo test -p voom-cli --features functional estimate_calibrate -- --test-threads=1
cargo test -p voom-cli --features functional plugin_stats_query_does_not_mutate
```

## Validation commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Out of scope

- Refactoring the rest of the estimate command.
- Adding stricter linting against `BootstrapResult { store, .. }`
  destructuring patterns (suggestion for a separate hygiene PR).
