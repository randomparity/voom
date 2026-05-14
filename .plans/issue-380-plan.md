# Plan: Mark voom-domain stats types as #[non_exhaustive] (#380)

## Problem

Four new public types in `crates/voom-domain/src/plugin_stats.rs` were
added without `#[non_exhaustive]`:

- `PluginInvocationOutcome` (enum, four variants)
- `PluginStatRecord` (struct, five fields)
- `PluginStatsRollup` (struct, ten fields)
- `PluginStatsFilter` (struct, three fields)

As soon as anything outside `voom-domain` starts constructing these by
name (CLI today, WASM plugin bindings tomorrow), adding a field becomes
a breaking change.

## Approach

Add `#[non_exhaustive]` to all four types. Provide named constructors so
external (and internal) callers can keep building values cleanly.

`#[non_exhaustive]` on enums also blocks exhaustive `match` from outside
the crate. The two known exhaustive matches today are in:

- `crates/voom-kernel/src/bus.rs` — uses `outcome_to_sql` analog
  inline; classifier returns `Ok`/`Skipped`/`Err{category}`/`Panic`.
- `plugins/sqlite-store/src/store/plugin_stats_storage.rs` — outcome
  encode/decode.

Both will need a catch-all `_ =>` arm. (The kernel's `error_category`
already has one for `VoomError`.) Acceptable: an unknown outcome maps
to a safe default ("unknown" string for SQL, log+skip for kernel).

## Changes

### `crates/voom-domain/src/plugin_stats.rs`

1. Add `#[non_exhaustive]` to all four types.
2. Add `PluginStatRecord::new(...)` and (optional) `PluginStatsRollup::new()`
   constructors so internal call sites that currently build by struct
   literal can switch (or be left alone since this crate has full
   field-access).
3. Add `PluginStatsFilter::new()` (already `#[derive(Default)]` — keep).

### `crates/voom-kernel/src/bus.rs`

Already has catch-all arms (`_ => "unknown"`); verify no exhaustive
match on `PluginInvocationOutcome` from outside the crate is broken.
Internal matches inside the kernel still see all variants because the
kernel depends on voom-domain and patterns are not affected by
`non_exhaustive` from outside the defining crate. Wait — kernel is NOT
the defining crate, so kernel's matches ARE affected. Confirm the
existing `match` statements compile; add `_ =>` if necessary.

### `plugins/sqlite-store/src/store/plugin_stats_storage.rs`

Same as kernel: it's a separate crate consuming `voom-domain` types.
Existing matches on `PluginInvocationOutcome` need catch-all arms.
`outcome_to_sql` already has one (`_ => "unknown"` per the issue).

### `plugins/sqlite-store/src/stats_sink.rs`

The test helper `rec()` builds a `PluginStatRecord` by struct literal.
External crate construction of a `#[non_exhaustive]` struct is forbidden
when there are private fields, but with all-public fields the only
restriction is that struct-literal syntax requires `..Default::default()`
or a named constructor. We add `PluginStatRecord::new(...)` and switch
the test helper to use it.

Similarly for `voom-cli/src/commands/plugin_stats.rs` (constructs
`PluginStatsFilter` by struct literal):

```rust
let filter = PluginStatsFilter {
    plugin: args.plugin,
    since,
    top: args.top,
};
```

→ replace with `PluginStatsFilter::new(args.plugin, since, args.top)`
or, more idiomatically, `PluginStatsFilter { plugin: …, since: …,
top: …, ..PluginStatsFilter::default() }`.

Same pattern in `voom-cli/tests/plugin_stats_e2e.rs`.

## Caveat / risk

The Rust rule: `#[non_exhaustive]` on a struct with public fields means
external callers cannot use struct-literal construction (`Type { … }`)
without functional-update syntax `..Default::default()`. Internal
(in-crate) callers are unaffected. The fix is mechanical: add `new`
constructors and update external callers.

## Test plan

- `cargo test --workspace` — verifies all match arms still compile.
- `cargo test -p voom-cli` — covers the CLI's filter construction.
- `cargo test -p voom-sqlite-store stats_sink::` — covers the test helper.

## Validation commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p voom-cli --features functional -- --test-threads=4
```

## Out of scope

- Adding `#[non_exhaustive]` to other public domain types not flagged
  in #380.
- Changing the serde representation of any type.
