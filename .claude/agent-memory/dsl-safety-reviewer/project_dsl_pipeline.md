---
name: DSL Pipeline Architecture
description: Key facts about the voom-dsl pipeline stages, panic sites, and known invariants
type: project
---

The DSL pipeline is: pest grammar → CST → AST (parser.rs) → validation (validator.rs) → compilation (compiler.rs) → pretty-printing (formatter.rs).

**Key invariants:**
- `compile_ast` is `pub(crate)` — only reachable externally through `compile_policy`, which always runs validation first
- `parser.rs` and `compiler.rs` both have `#![allow(clippy::unwrap_used)]` with documented justifications
- Grammar guarantees `run_if_trigger` is only "modified" | "completed" — the `unreachable!` in compiler.rs line 102 is correct
- Grammar guarantees `actions_op` target is "audio" | "subtitle" | "video" — `build_actions` text-split is safe
- `parse_track_target` has `unreachable!` fallback — safe because validator runs first
- `CompiledPhase` is `#[non_exhaustive]` but struct literal construction in compiler.rs is in-crate, so it's allowed
- Grammar enforces `phase+` (at least one phase required)
- Size limit: `MAX_POLICY_SIZE = 1_024 * 1_024` (1 MiB) checked at parse time
- Nesting depth limit: `MAX_NESTING_DEPTH = 100` for conditions and filters

**Why:** The grammar rules constrain values to finite keyword sets; the `unreachable!` arms are compile-time documented contracts.
**How to apply:** When auditing `unreachable!` in compiler.rs, verify the corresponding grammar rule exists and is a named rule (so it's constrained). If a new `unreachable!` is added, confirm it has a grammar rule backing it.

**Service API distinction:**
- `validate_source` (service.rs): runs parse + validate ONLY. Does NOT run compile_ast.
  - Used by web server `/api/policy/validate` endpoint
  - Can return valid=true for policies with invalid regex in `title matches` filters
- `compile_policy` (lib.rs): runs full pipeline (parse + validate + compile)
  - Used by CLI `voom policy validate`, `voom process`, etc.
  - Always correct
- `format_source` (service.rs): runs parse + format. No validation.

**CompiledPolicy serde safety:**
- All fields are `pub` — external code can mutate to violate invariants
- `CompiledRegex::Deserialize` re-compiles regex, fails on invalid patterns (safe)
- `phase_order` referencing non-existent phases is not caught by deserialization (consumer must validate)
- This is acceptable since CompiledPolicy is an internal crate type

**Topological sort:**
- Uses Kahn's algorithm (BFS-based) with sorted queue for determinism
- `queue.insert(pos, neighbor)` is O(n) per insertion — quadratic for large graphs but acceptable (policies are small)
- Error message "cannot determine phase order due to circular dependencies" is misleading when the real cause is unknown phase references (protected by validator)

**Formatter round-trip gaps (found 2026-03-29):**
- `keep_backups: false` is silently dropped by formatter (only `Some(true)` is emitted)
  Round-trip failure: `Some(false)` → formatted → `None`
- Comments are NOT preserved (pest strips them) — documented limitation
