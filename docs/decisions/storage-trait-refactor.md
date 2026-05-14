# Decision: StorageTrait supertrait list refactor (#381)

**Status:** decided — direction A (status quo + docs), with B (macro)
flagged as a low-risk follow-up.

**Owner:** issue #381.

**Date:** 2026-05-14.

## Problem

`crates/voom-domain/src/storage.rs::StorageTrait` is a composition trait
that requires every concrete storage backend (currently `SqliteStore`
plus the `InMemoryStore` test fixture) to implement 16 per-table
storage traits. After #92's `PluginStatsStorage` was added the bound
list is:

```rust
pub trait StorageTrait:
    FileStorage
    + JobStorage
    + PlanStorage
    + FileTransitionStorage
    + PluginDataStorage
    + BadFileStorage
    + MaintenanceStorage
    + HealthCheckStorage
    + EventLogStorage
    + SnapshotStorage
    + PendingOpsStorage
    + VerificationStorage
    + TranscodeOutcomeStorage
    + EstimateStorage
    + ScanSessionMutationStorage
    + PluginStatsStorage
{ ... }
```

A blanket `impl<T> StorageTrait for T where ...` repeats the same list.
Every new feature requiring persistence adds another trait to both
places. The issue called out three concerns:

1. **Cognitive load** — reviewers can't tell which methods are "core"
   vs. specialized.
2. **Object-safety friction** — adding a method with generics anywhere
   in the supertrait chain breaks `dyn StorageTrait` for everyone.
3. **Test mock burden** — `InMemoryStore` must implement every trait,
   even for features its test doesn't exercise.

## Options compared

| Option | Summary | LOC impact | Call-site impact | Runtime |
|--------|---------|------------|------------------|---------|
| **A. Status quo + docs** | Document the pattern as intentional. Add a comment in `storage.rs` referencing this decision doc. | 0 | 0 | 0 |
| **B. Macro for supertrait list** | A `storage_traits!()` macro emits both lines from a single source of truth. | ≈ -32 (16 lines × 2 places) plus ~20 for the macro definition | 0 | 0 |
| **C. Core trait + extensions** | Split into `CoreStorage` (5-6 most-used) + opt-in extension traits (e.g. `dyn CoreStorage + dyn PluginStatsStorage`). | ~+200 net, mostly call-site refactors | High — every `dyn StorageTrait` caller must decide which extensions it requires. ~13 files affected. | 0 |
| **D. `dyn Any` side-channel** | `fn cap<T: 'static>(&self) -> Option<&T>` lets backends expose extension traits dynamically. | ~+100 for cap plumbing | Medium — every opt-in call adds a downcast | 1 downcast per opt-in lookup |

### LOC notes

- Option A: zero source LOC; the decision doc itself is ~150 lines.
- Option B: collapses the 16-line `StorageTrait` declaration and the
  16-line blanket impl into a single macro invocation. The macro
  definition itself is small (a list and two expansions). Net win
  ≈ 12 lines, but more importantly the *single source of truth*
  removes the risk of the two lists diverging.
- Option C: most invasive. Each `dyn StorageTrait` caller has to declare
  which extension traits it depends on. Search shows 13 files use
  `dyn StorageTrait` today (CLI test harness, scan/process pipelines,
  web-server, ffmpeg/mkvtoolnix executors, verifier, report, etc.).
  Some of these — e.g. CLI commands — touch many storage facets and
  would end up depending on most extensions anyway.
- Option D: surface-area-light at the trait level, but every call site
  pays the type-system tax (downcast + None handling). Worst of both
  worlds for our usage pattern.

### Call-site impact

- Option A: none.
- Option B: none — the macro emits the same trait that exists today.
- Option C: high. Every caller of `Arc<dyn StorageTrait>` needs to
  either keep the full bound (defeating the refactor) or rewrite as
  `Arc<dyn CoreStorage + ExtAlpha + ExtBeta + ...>`. Mocks gain less
  than expected because the most exercised storage methods cluster in
  `FileStorage` + `PlanStorage` + `EventLogStorage`, which most tests
  need.
- Option D: medium. `store.cap::<dyn PluginStatsStorage>().unwrap()`
  per call site is ugly enough to cluster into helper functions, which
  then re-export the typed trait — undoing the benefit.

### Runtime cost

- A, B, C: zero — same static dispatch.
- D: one downcast per opt-in method call. Negligible in absolute
  terms; but it's a regression vs. zero.

## Recommendation

**Adopt Option A** (status quo + docs) **for #381 itself**. Add an
authoritative comment in `crates/voom-domain/src/storage.rs` pointing
to this decision document and explaining that the supertrait list is
intentional.

**Consider Option B (macro) as a future low-risk improvement** if the
two lists drift or the supertrait count crosses ~20. File a fresh
issue when that becomes a real maintenance pain point — Option B is
mechanical and cheap to do later.

**Reject Option C and Option D for now.** They are correct in
principle but their downstream blast radius is incommensurate with
the current pain. Revisit if/when:

- The `InMemoryStore` mock implementation burden becomes a measured
  productivity tax.
- A storage extension lands that genuinely doesn't apply to most
  callers (e.g. an experimental backend).
- The supertrait list crosses 20 and a typo or omission causes a
  real bug.

## Why this isn't a refactor PR

The issue explicitly asks for *a design doc, not a refactor*: "Pick
a direction in a design doc before any refactor." That's what this
delivers. The recommendation is to keep the current structure — the
right next step is a small Option B refactor when warranted, on its
own issue, with its own review.

## References

- Issue: #381.
- Composing trait: `crates/voom-domain/src/storage.rs::StorageTrait`.
- Concrete impl: `plugins/sqlite-store/src/store/`.
- Test fixture: `crates/voom-domain/src/test_support.rs`.
- Original sin: introduced incrementally since the first storage
  refactor; current list dates to #92 (added `PluginStatsStorage`).
