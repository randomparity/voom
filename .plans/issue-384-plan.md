# Plan: Deterministic stats_sink overflow test (#384)

## Problem

`overflow_increments_dropped_counter_when_channel_full` in
`plugins/sqlite-store/src/stats_sink.rs` uses a `capacity=0` rendezvous
channel plus a real writer thread, and relies on scheduling to observe at
least one `try_send` failure. It already flaked once (tightened to
`>= 150/200`, then weakened to `> 0`). Codex stop-time review flagged
that even the weakened form is scheduling-dependent — there is no
mathematical guarantee.

## Approach

Add a test seam constructor that does **not** spawn the writer thread.
The test holds the receiver alive in scope but never calls `recv`. With a
small capacity (e.g. 4), the channel fills after 4 sends, and every
subsequent `try_send` deterministically returns `Err(Full)` because no
consumer is draining. The test asserts the exact drop count.

## Changes

### `plugins/sqlite-store/src/stats_sink.rs`

1. Add a test-only constructor (cfg(test)):

   ```rust
   #[cfg(test)]
   impl SqliteStatsSink {
       /// Test-only: build a sink with a held receiver and NO writer
       /// thread. Caller is responsible for keeping the returned
       /// `Receiver` alive so the channel doesn't `Disconnected`.
       /// `dropped_count` increments deterministically once the channel
       /// fills.
       pub(crate) fn with_held_receiver_for_tests(
           capacity: usize,
       ) -> (Self, std::sync::mpsc::Receiver<PluginStatRecord>) {
           let (tx, rx) = sync_channel::<PluginStatRecord>(capacity);
           let dropped = Arc::new(AtomicU64::new(0));
           let failed_flushes = Arc::new(AtomicU64::new(0));
           let evicted = Arc::new(AtomicU64::new(0));
           let sink = Self {
               tx: Some(tx),
               dropped,
               failed_flushes,
               evicted,
               writer: None, // no writer thread
           };
           (sink, rx)
       }
   }
   ```

   Note: the existing `Drop` impl is `if let Some(h) = self.writer.take()`
   — already gracefully handles `writer: None`, so no Drop change needed.

2. Rewrite the flaky test:

   ```rust
   #[test]
   fn overflow_increments_dropped_counter_when_channel_full() {
       const CAP: usize = 4;
       const EXTRA: u64 = 10;
       let (sink, _rx) = SqliteStatsSink::with_held_receiver_for_tests(CAP);
       // Fill the channel exactly to capacity.
       for i in 0..CAP as u64 {
           sink.record(rec(i));
       }
       assert_eq!(
           sink.dropped_count(),
           0,
           "no drops expected while the channel still has slots"
       );
       // Every subsequent send must fail: no consumer is draining and
       // capacity is full.
       for i in 0..EXTRA {
           sink.record(rec(CAP as u64 + i));
       }
       assert_eq!(
           sink.dropped_count(),
           EXTRA,
           "expected exactly EXTRA drops with no consumer draining"
       );
       // `_rx` kept alive deliberately: dropping it would change the
       // failure from `Full` (counted as a drop) to `Disconnected` (also
       // counted as a drop, but no longer a true overflow test).
       drop(_rx);
   }
   ```

3. Keep `channel_overflow_does_not_panic` as the no-assertion stress
   test for the production constructor (already does no claim about the
   counter).

4. Remove `with_capacity_for_tests` once nothing else uses it. (Currently
   only the now-replaced test calls it. Verify with `rg`.)

## Edge cases

- `writer: None` in Drop. The existing `Drop` impl already handles None
  via `if let Some(h) = self.writer.take()`. Confirmed by reading source.
- Test runs independently of scheduler speed. Channel capacity is fixed;
  sends are synchronous; receiver is never touched. No flake surface.
- Ordering: the `tx.try_send` returns `Err(Full)` (not `Disconnected`)
  while `_rx` is alive. After `drop(_rx)` at the end, no further sends
  happen. The bookkeeping path `is_err() → fetch_add` is identical for
  both error variants.

## Test plan

- Run the new test 50× to confirm determinism:
  `for i in {1..50}; do cargo test -p voom-sqlite-store
   overflow_increments_dropped_counter_when_channel_full -- --exact ||
   exit 1; done`
- Run the existing test file fully:
  `cargo test -p voom-sqlite-store stats_sink::`.
- Run the full workspace:
  `cargo test --workspace`.

## Validation commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Out of scope

- Refactoring `SqliteStatsSink`'s production constructor.
- Changing the `dropped_count` semantics or the drop-on-Disconnected
  policy.
- Adding `with_held_receiver_for_tests` to non-test code paths.
