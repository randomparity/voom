# Mutation Testing Baseline

This document captures the current `cargo mutants` baseline for the three logic-dense crates targeted by issue #214. The numbers below are the reference point that follow-up triage work needs to drive down. The original baseline (SHA `dd69cfa`, 2026-05-05) has been replaced in place by the post-Phase-2 numbers from issue #236; the `Source SHA` field below identifies the snapshot the counts correspond to.

| | |
|---|---|
| **Captured** | 2026-05-05 |
| **Workflow run** | Local re-baseline on `homer.pdx.drc.nz` (no GitHub Actions URL) |
| **Source SHA** | `5363860` |
| **cargo-mutants version** | 27.0.0 |
| **Config** | `.cargo/mutants.toml` (`exclude_globs = ["**/tests/**", "**/benches/**", "**/examples/**"]`) |
| **Per-mutant timeout** | 300 s, passed via `--timeout 300` in the workflow |

## Per-crate counts

`Caught` mutants were detected by the test suite. `Missed` mutants survived all tests — these are the targets for triage. `Unviable` mutants failed to compile (cargo-mutants writes them out, the test suite never sees them). `Success` is the unmutated-tree sanity run; it is not a mutant outcome and is excluded from the totals.

| Crate | Total mutants | Caught | Missed | Unviable | Catch rate |
|---|---:|---:|---:|---:|---:|
| `voom-dsl` | 518 | 392 | 58 | 68 | 87.1 % |
| `voom-policy-evaluator` | 265 | 217 | 42 | 6 | 83.8 % |
| `voom-phase-orchestrator`* | 11 | 5 | 0 | 6 | 100.0 % |
| **Total** | **794** | **614** | **100** | **80** | **86.0 %** |

\* not re-run for Phase 2; logic unchanged since the original baseline.

Catch rate is `Caught / (Caught + Missed)` — `Unviable` mutants are excluded because they never reach the test suite.

`voom-phase-orchestrator` already catches every viable mutant; its 11 mutants live in a single `lib.rs` whose DAG-resolution logic is exercised by the existing depends_on / skip when / run_if test matrix. Triage work concentrates on the other two crates. After issue #236's Phase 2 (PR for branch `test/issue-236-mutation-triage-phase2`), both `voom-dsl` and `voom-policy-evaluator` exceed the 80% catch-rate threshold defined in #236's Definition of Done.

Progression across the three baselines captured to date:

| Crate | Pre-Phase-1 (`dd69cfa`) | Post-Phase-1 (`3da0275`) | Post-Phase-2 (`5363860`) |
|---|---:|---:|---:|
| `voom-dsl` | 61.8 % | 70.7 % | **87.1 %** |
| `voom-policy-evaluator` | 50.3 % | 73.0 % | **83.8 %** |

## Representative surviving mutants

The first 10 surviving mutants per crate, taken in declaration order from `outcomes.json`. The full lists are in each workflow run's `mutants-<crate>` artifact (90-day retention) under `mutants.out/missed.txt` and `mutants.out/outcomes.json`.

### `voom-dsl` (58 surviving)

- `crates/voom-dsl/src/compiled.rs:52:9: replace <impl fmt::Debug for CompiledRegex>::fmt -> fmt::Result with Ok(Default::default())`
- `crates/voom-dsl/src/compiler.rs:171:38: replace == with != in compile_operation`
- `crates/voom-dsl/src/compiler.rs:218:21: delete match arm "all" in compile_operation`
- `crates/voom-dsl/src/compiler.rs:318:21: delete match arm Value::Number(n, _) in compile_synthesize`
- `crates/voom-dsl/src/compiler.rs:319:21: delete match arm Value::Ident(s) | Value::String(s) in compile_synthesize`
- `crates/voom-dsl/src/compiler.rs:329:38: replace == with != in compile_synthesize`
- `crates/voom-dsl/src/compiler.rs:337:21: delete match arm Value::Number(n, _) in compile_synthesize`
- `crates/voom-dsl/src/compiler.rs:338:21: delete match arm Value::Ident(s) in compile_synthesize`
- `crates/voom-dsl/src/compiler.rs:541:9: delete match arm "track" in parse_track_target`
- `crates/voom-dsl/src/compiler.rs:590:60: replace > with == in topological_sort`

What remains in `voom-dsl` after Phase 2 is concentrated in the formatter and the compiler's synthesize/topological-sort paths. The largest remaining clusters are `format_operation` (6 survivors) and `format_number`, `compile_synthesize`, `topological_sort`, and `validator::broad_track_category` (5 each), with `format_when` and `format_policy` trailing at 4 apiece. The formatter clusters reflect the fact that round-trip / golden-output tests cover the common cases but not every per-operation rendering branch; the `compile_synthesize` cluster is missing coverage for individual `Value::*` arms feeding the synth-track parameters.

### `voom-policy-evaluator` (42 surviving)

- `plugins/policy-evaluator/src/lib.rs:71:5: replace evaluate_single_phase_with_hints -> Option<voom_domain::plan::Plan> with None`
- `plugins/policy-evaluator/src/condition.rs:258:9: delete match arm (serde_json::Value::Number(l), serde_json::Value::Number(r)) in json_values_equal`
- `plugins/policy-evaluator/src/condition.rs:258:84: replace == with != in json_values_equal`
- `plugins/policy-evaluator/src/container_compat.rs:42:9: delete match arm Container::Mov | Container::Ts | Container::Flv | Container::Wmv | Container::Other in codec_supported`
- `plugins/policy-evaluator/src/evaluator.rs:266:33: replace == with != in apply_safeguards`
- `plugins/policy-evaluator/src/evaluator.rs:272:8: delete ! in apply_safeguards`
- `plugins/policy-evaluator/src/evaluator.rs:386:37: replace > with == in apply_container_safeguard`
- `plugins/policy-evaluator/src/evaluator.rs:386:37: replace > with >= in apply_container_safeguard`
- `plugins/policy-evaluator/src/evaluator.rs:410:5: replace apply_safeguard_for_track_type with ()`
- `plugins/policy-evaluator/src/evaluator.rs:415:14: replace == with != in apply_safeguard_for_track_type`

The dominant remaining cluster is `apply_safeguard_for_track_type` in `evaluator.rs` with 9 survivors, followed by `is_font_attachment` in `filter.rs` (5) and the `apply_safeguards` predicate cluster in `evaluator.rs` (3). The safeguard clusters indicate that the per-track-type guard branches and their boundary comparisons aren't yet exercised end-to-end; `is_font_attachment` is a small classification helper whose individual MIME/extension branches are uncovered.

### `voom-phase-orchestrator`

No surviving mutants.

## Triage

Issue [#236 — track triage of cargo-mutants survivors](https://github.com/randomparity/voom/issues/236) drove the Phase 1 and Phase 2 work that produced the catch rates above and was closed alongside this re-baseline. The clusters listed above are candidates for a follow-up triage issue if the project decides to push catch rates further.

The general approach for triaging a survivor:

1. Read the survivor's `name` field — it includes the file, line, and the exact mutation cargo-mutants applied.
2. Write a focused unit test that distinguishes the original code from the mutant. If the mutation is a comparison operator flip, the test should hit the boundary value; if it is a deleted match arm, the test should pass that arm's input.
3. Verify locally with `cargo mutants -p <crate> --file <relative-path> --regex <fragment of the mutation name>`.
4. Group several survivors into a single PR when they touch related code (e.g. the nine `apply_safeguard_for_track_type` mutants in `policy-evaluator/src/evaluator.rs` belong together).

If a mutant is provably equivalent (no observable behavior change), document the reasoning in the test file rather than suppressing it via `cargo-mutants` config — that keeps the analysis reviewable and discoverable.

## Refreshing the baseline

The numbers above will drift as code changes. Tomorrow's nightly run will already have a different `Source SHA`. Update this document only when meaningful work has shifted the numbers (e.g. a triage PR that catches a cluster of survivors). For day-to-day reference, prefer the latest run on the [Actions / Mutants page](https://github.com/randomparity/voom/actions/workflows/mutants.yml).
