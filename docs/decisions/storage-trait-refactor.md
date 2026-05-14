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

## Caller inventory

A grep of the actual repo gives 23 Rust files that use
`dyn StorageTrait`, not the 13 quoted in the first revision of this
doc:

```
git grep -l -E "dyn ([[:alnum:]_:]+::)*StorageTrait" -- '*.rs' | wc -l
# → 23
```

The list (as of branch `docs/issue-381-storage-trait-refactor`):

- `crates/voom-cli/src/app.rs`
- `crates/voom-cli/src/commands/events.rs`
- `crates/voom-cli/src/commands/process/mod.rs`
- `crates/voom-cli/src/commands/process/pipeline_streaming.rs`
- `crates/voom-cli/src/commands/report.rs`
- `crates/voom-cli/src/commands/scan/mod.rs`
- `crates/voom-cli/src/commands/scan/pipeline.rs`
- `crates/voom-cli/src/commands/serve.rs`
- `crates/voom-cli/src/commands/verify.rs`
- `crates/voom-cli/src/introspect.rs`
- `crates/voom-cli/src/recovery.rs`
- `crates/voom-cli/src/retention.rs`
- `crates/voom-cli/tests/common/mod.rs`
- `crates/voom-kernel/src/host/store.rs`
- `crates/voom-kernel/src/loader.rs`
- `plugins/ffmpeg-executor/src/lib.rs`
- `plugins/mkvtoolnix-executor/src/lib.rs`
- `plugins/report/src/lib.rs`
- `plugins/report/src/query.rs`
- `plugins/verifier/src/lib.rs`
- `plugins/web-server/src/server.rs`
- `plugins/web-server/src/state.rs`
- `plugins/web-server/src/templates.rs`

This expands Option C's blast radius materially over the v1 estimate.

## Options compared

| Option | Summary | LOC impact | Call-site impact | Runtime |
|--------|---------|------------|------------------|---------|
| **A. Status quo + docs** | Document the pattern as intentional. Add a comment in `storage.rs` referencing this decision doc. | 0 | 0 | 0 |
| **B. Macro for supertrait list** | A `storage_traits!()` macro emits both the trait declaration and the blanket impl from a single source of truth. | ≈ break-even after macro definition; main win is removing the risk of the two lists drifting. | 0 — the macro emits the same `StorageTrait`. | 0 |
| **C. Composite trait families** | Define a small `CoreStorage` composite (5–6 most-used sub-traits) and let call sites that need more compose by declaring **new named composite traits** (e.g. `trait ReportStorage: CoreStorage + EstimateStorage + ...`). Rust forbids `dyn A + dyn B` for two non-auto traits in one trait object (rustc E0225) — every grouping has to be a real named trait, so the design ends up with a forest of composites rather than ad-hoc combinations. | ~+200 net assuming ≤5 composites and the CLI uses the full set; 23 caller files need decisions. | High. Most CLI commands touch many sub-traits and would end up depending on the maximal composite (defeating the refactor). | 0 |
| **D. Object-safe extension downcast** | Add an `as_any: &dyn std::any::Any` accessor on a smaller core trait, and per-extension `Option<&dyn Ext>` getters backed by `TypeId`. (A generic `fn cap<T: 'static>` is *not* object-safe — rustc E0038 — so the typed lookup lives in helper functions, not on the trait.) | ~+100 for cap plumbing + one accessor per extension. | Medium — every opt-in call goes through `store.as_pluginstats().unwrap_or(...)`. | One downcast per opt-in lookup. |

### LOC notes

- Option A: zero source LOC; the decision doc itself is ~200 lines.
- Option B: collapses the 16-line `StorageTrait` declaration and the
  16-line blanket impl into a single macro invocation. The macro
  definition itself is small (a list and two expansions). Net change
  is roughly break-even but the *single source of truth* removes the
  risk of the two lists diverging.
- Option C: most invasive. Each `dyn StorageTrait` caller has to declare
  which composite trait it depends on. With 23 files and a long tail
  of cross-cutting CLI commands, most callers would land back on the
  full composite (defeating the refactor). Adding composites also
  forces a decision matrix for every new sub-trait — "which
  composites should I go in?".
- Option D: surface-area-light at the trait level but every call site
  pays the type-system tax (downcast + None handling) and the cost
  multiplier compounds in long methods that touch several extensions.

### Why Options C and D were written incorrectly in v1

The first revision of this doc described Option C as
`dyn CoreStorage + dyn PluginStatsStorage` and Option D as
`fn cap<T: 'static>(&self) -> Option<&T>`. Neither compiles as written:

- Rust trait objects allow only **one** non-auto trait in a single
  `dyn` expression — additional non-auto traits must be folded into a
  new composite trait. (rustc `--explain E0225`.)
- A generic trait method (e.g. `fn cap<T: 'static>`) is not
  dyn-compatible unless `Self: Sized`, which would make it
  unavailable on the `dyn StorageTrait` callers this option targets.
  (rustc `--explain E0038`.)

The Option C / D descriptions above have been corrected to use
compiling shapes: explicit named composites for C, object-safe
`as_any` + typed accessors for D.

## Recommendation

**Adopt Option A** (status quo + docs) **for #381 itself**. Add an
authoritative comment in `crates/voom-domain/src/storage.rs` pointing
to this decision document and explaining that the supertrait list is
intentional.

**Consider Option B (macro) as a future low-risk improvement** if the
two lists drift or the supertrait count crosses ~20. File a fresh
issue when that becomes a real maintenance pain point — Option B is
mechanical and cheap to do later.

**Reject Option C and Option D for now.** They are correct in principle
but their downstream blast radius is incommensurate with the current
pain. With 23 `dyn StorageTrait` callers spanning the CLI, kernel,
plugins, and web UI, Option C's named-composite refactor would touch
every layer simultaneously; Option D's downcast tax compounds in the
long-method CLI flows. Revisit if/when:

- The `InMemoryStore` mock implementation burden becomes a measured
  productivity tax.
- A storage extension lands that genuinely doesn't apply to most
  callers (e.g. an experimental backend).
- The supertrait list crosses 20 and a typo or omission causes a real
  bug.

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
- Rust trait-object rules: rustc `--explain E0225` (one non-auto
  trait per `dyn`), `--explain E0038` (generic methods + dyn-safety).
- Caller inventory (23 files): see `git grep` invocation above.
