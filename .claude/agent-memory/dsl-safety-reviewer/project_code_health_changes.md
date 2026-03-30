---
name: Code-Health Branch DSL Changes Summary
description: Summary of DSL changes in the desloppify/code-health branch (reviewed 2026-03-28)
type: project
---

**Branch:** desloppify/code-health

**Changes audited:**
1. `CompiledPolicy` and related IR types moved from `voom_domain::compiled` to `voom_dsl::compiled` — correct, all consumers updated to `voom_dsl::compiled::*`
2. `regex` dep moved from voom-domain to voom-dsl — correct, follows the type move
3. `CompiledPhase::new()` constructor removed, replaced with struct literal — safe (in-crate, `#[non_exhaustive]` allows it)
4. `compile_transcode` extracted as standalone infallible function — correct, returns `CompiledOperation` not `Result`
5. `format_transcode` and `format_rules` extracted from `format_operation` — functionally identical, no regression
6. `format!` → `write!/writeln!` in formatter — cosmetic, `let _ =` silences the infallible write-to-String error. Correct.
7. `run_if` compiler uses explicit `unreachable!` for non-"modified"|"completed" triggers — safe, grammar enforces it
8. Grammar: `lang_target` and `run_if_trigger` extracted as named rules — parser updated to use `.find()` on named rules instead of text contains/if-else. More correct and maintainable.
9. Container validation: hardcoded list replaced with `Container::from_extension()` — expands accepted containers (adds flv, wmv, mka, mks, m4v, m4a, m2ts, mts). Case-insensitive now.
10. `build_value`: explicit `Rule::ident` arm removed, collapsed into `_ =>` wildcard — functionally identical
11. `build_config` and several build_* functions: return type changed from `Result<T>` to `T` (infallible) — correct, operations can't fail
12. `#[allow(clippy::missing_errors_doc)]` removed from lib.rs — workspace lints now cover this
13. `leading_keyword()` helper function extracted — reduces code duplication, functionally equivalent

**No regressions found. All 25 tests + 3 doc-tests pass.**

**Why:** Code health cleanup sprint. Moving CompiledPolicy to voom-dsl reduces voom-domain's scope and eliminates a cross-crate dependency on `regex`.
**How to apply:** These patterns (named grammar rules for constrained alternations, infallible build_* functions, extracted format helpers) are the new standard — follow them when adding new grammar rules.
