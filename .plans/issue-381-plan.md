# Plan: StorageTrait supertrait refactor design doc (#381)

## Problem

`StorageTrait` is a composition trait with 16 supertraits. Every new
feature that needs persistence requires adding another supertrait to
both the trait def and the blanket impl. Concerns: cognitive load,
object-safety friction, test-mock burden (InMemoryStore must implement
all 16 for every test).

The issue's `Ask` is a **design doc**, not a refactor: compare options
for LOC impact, downstream call-site impact, and runtime cost, then
pick a direction.

## Approach

Write `docs/decisions/storage-trait-refactor.md` (or similar) comparing:

| Option | Summary | LOC impact | Call-site impact | Runtime |
|--------|---------|------------|------------------|---------|
| A. Status quo + docs | Document the pattern as intentional. | 0 | 0 | 0 |
| B. Macro for supertrait list | Auto-generate the 16-line bound list from a `storage_traits!` macro. | -32 (16 lines × 2 places) | 0 | 0 |
| C. Core trait + extensions | Split into `CoreStorage` (5–6 most-used) + opt-in extension traits. | +200, mostly call-site refactors | High (every `dyn StorageTrait` caller). | 0 |
| D. `dyn Any` side-channel | `fn cap<T: 'static>() -> Option<&T>` for optional traits. | +100 for cap plumbing | Medium | 1 downcast per opt-in lookup |

The doc should:

- Walk each option's pros/cons.
- Survey existing `dyn StorageTrait` callers (Phase 1 — `rg "dyn StorageTrait"`).
- Recommend one direction. Recommended direction: **Option A (Status quo
  + docs)** in the immediate term, with **Option B (macro)** as the
  low-risk improvement to consider next.
- Note: Option C is the principled long-term answer but its blast
  radius is too large to do in the same PR as the doc; tracked as a
  separate issue once the supertrait list crosses 20+ or once
  `InMemoryStore`'s implementation burden becomes a measured pain point.

## Why not implement a refactor here

The issue says: "Spike each option enough to compare LOC impact,
downstream call-site impact, and runtime cost. Pick a direction in a
design doc before any refactor." So #381 IS the design doc. The
refactor itself is a separate follow-up.

## Affected files

| File | Change |
|------|--------|
| `docs/decisions/storage-trait-refactor.md` (NEW) | Comparison doc + recommendation. |
| `docs/architecture.md` | One-paragraph pointer to the decision doc in the "Storage" section. |
| `crates/voom-domain/src/storage.rs` | (Optional) Doc-comment on the `StorageTrait` definition pointing at the decision doc, explaining why the supertrait list is intentional. |

## Acceptance

- [ ] Decision document exists at `docs/decisions/`.
- [ ] Document compares all four options on the three axes the issue
  named (LOC, call-site, runtime).
- [ ] Document recommends a direction with explicit reasoning.
- [ ] Architecture doc points at the decision.
- [ ] No code changes that observably affect `voom-domain` callers.

## Test plan

No code changes ⇒ no new tests. Run `cargo test --workspace` to verify
nothing broke (doc-only PR).

## Validation commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Out of scope

- Implementing the refactor.
- Adding `#[non_exhaustive]` to traits (different concern; #380 covers
  data types).
