# Mutation Testing Baseline

This document captures the initial `cargo mutants` baseline for the three logic-dense crates targeted by issue #214. The numbers below are the reference point that follow-up triage work needs to drive down.

| | |
|---|---|
| **Captured** | 2026-05-05 |
| **Workflow run** | [actions/runs/25374530315](https://github.com/randomparity/voom/actions/runs/25374530315) (`workflow_dispatch` on `main`) |
| **Source SHA** | `dd69cfa` |
| **cargo-mutants version** | 27.0.0 |
| **Config** | `.cargo/mutants.toml` (`exclude_globs = ["**/tests/**", "**/benches/**", "**/examples/**"]`) |
| **Per-mutant timeout** | 300 s, passed via `--timeout 300` in the workflow |

## Per-crate counts

`Caught` mutants were detected by the test suite. `Missed` mutants survived all tests — these are the targets for triage. `Unviable` mutants failed to compile (cargo-mutants writes them out, the test suite never sees them). `Success` is the unmutated-tree sanity run; it is not a mutant outcome and is excluded from the totals.

| Crate | Total mutants | Caught | Missed | Unviable | Catch rate |
|---|---:|---:|---:|---:|---:|
| `voom-dsl` | 514 | 197 | 122 | 195 | 61.8 % |
| `voom-policy-evaluator` | 265 | 74 | 73 | 118 | 50.3 % |
| `voom-phase-orchestrator` | 11 | 5 | 0 | 6 | 100.0 % |
| **Total** | **790** | **276** | **195** | **319** | **58.6 %** |

Catch rate is `Caught / (Caught + Missed)` — `Unviable` mutants are excluded because they never reach the test suite.

`voom-phase-orchestrator` already catches every viable mutant; its 11 mutants live in a single `lib.rs` whose DAG-resolution logic is exercised by the existing depends_on / skip when / run_if test matrix. Triage work concentrates on the other two crates.

## Representative surviving mutants

The first 10 surviving mutants per crate, taken in declaration order from `outcomes.json`. The full lists are in each workflow run's `mutants-<crate>` artifact (90-day retention) under `mutants.out/missed.txt` and `mutants.out/outcomes.json`.

### `voom-dsl` (122 surviving)

- `crates/voom-dsl/src/compiled.rs:46:9: replace CompiledRegex::pattern -> &str with ""`
- `crates/voom-dsl/src/compiled.rs:46:9: replace CompiledRegex::pattern -> &str with "xyzzy"`
- `crates/voom-dsl/src/compiler.rs:29:45: replace && with || in safe_u32`
- `crates/voom-dsl/src/compiler.rs:29:17: replace && with || in safe_u32`
- `crates/voom-dsl/src/compiler.rs:29:10: replace >= with < in safe_u32`
- `crates/voom-dsl/src/compiler.rs:29:22: replace <= with > in safe_u32`
- `crates/voom-dsl/src/compiler.rs:29:58: replace == with != in safe_u32`
- `crates/voom-dsl/src/compiler.rs:78:9: delete match arm "skip" in parse_error_strategy`
- `crates/voom-dsl/src/compiler.rs:90:9: delete match arm "first" in parse_default_strategy`
- `crates/voom-dsl/src/compiler.rs:91:9: delete match arm "all" in parse_default_strategy`

The cluster on `compiler.rs:29` is a single line — `safe_u32`'s range-check predicate. A handful of focused tests exercising u32 boundary values would knock out all five mutants on that line at once. `parse_error_strategy` / `parse_default_strategy` are similarly clustered: missing tests for each enum arm.

### `voom-policy-evaluator` (73 surviving)

- `plugins/policy-evaluator/src/condition.rs:59:40: replace && with || in evaluate_condition`
- `plugins/policy-evaluator/src/condition.rs:59:36: replace > with == in evaluate_condition`
- `plugins/policy-evaluator/src/condition.rs:59:36: replace > with < in evaluate_condition`
- `plugins/policy-evaluator/src/condition.rs:59:36: replace > with >= in evaluate_condition`
- `plugins/policy-evaluator/src/condition.rs:59:43: delete ! in evaluate_condition`
- `plugins/policy-evaluator/src/condition.rs:61:65: replace <= with > in evaluate_condition`
- `plugins/policy-evaluator/src/condition.rs:101:9: delete match arm "hwaccels" in resolve_system_field`
- `plugins/policy-evaluator/src/condition.rs:132:9: delete match arm "language" | "lang" in resolve_track_field`
- `plugins/policy-evaluator/src/condition.rs:133:9: delete match arm "title" in resolve_track_field`
- `plugins/policy-evaluator/src/condition.rs:134:9: delete match arm "channels" in resolve_track_field`

The `evaluate_condition` cluster on lines 59 and 61 is the predicate at the heart of policy evaluation — six surviving mutants on two lines means the comparison operators and boolean composition aren't being exercised end-to-end. The `resolve_*_field` mutants are missing test coverage for individual track/system field aliases.

### `voom-phase-orchestrator`

No surviving mutants.

## Triage

A single tracking issue gathers follow-up work to drive surviving-mutant counts down: see [#236 — track triage of cargo-mutants survivors](https://github.com/randomparity/voom/issues/236).

The general approach for triaging a survivor:

1. Read the survivor's `name` field — it includes the file, line, and the exact mutation cargo-mutants applied.
2. Write a focused unit test that distinguishes the original code from the mutant. If the mutation is a comparison operator flip, the test should hit the boundary value; if it is a deleted match arm, the test should pass that arm's input.
3. Verify locally with `cargo mutants -p <crate> --file <relative-path> --regex <fragment of the mutation name>`.
4. Group several survivors into a single PR when they touch related code (e.g. the five `safe_u32` mutants in `voom-dsl/compiler.rs:29` belong together).

If a mutant is provably equivalent (no observable behavior change), document the reasoning in the test file rather than suppressing it via `cargo-mutants` config — that keeps the analysis reviewable and discoverable.

## Refreshing the baseline

The numbers above will drift as code changes. Tomorrow's nightly run will already have a different `Source SHA`. Update this document only when meaningful work has shifted the numbers (e.g. a triage PR that catches a cluster of survivors). For day-to-day reference, prefer the latest run on the [Actions / Mutants page](https://github.com/randomparity/voom/actions/workflows/mutants.yml).
