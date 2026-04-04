# Doc-tests for Public API Functions

**Date:** 2026-04-04
**Issue:** #103

## Goal

Add runnable doc-tests to the 5 priority workspace crates, bringing the workspace from 3 passing / 3 ignored doc-tests to >20 passing / 0 ignored.

## Baseline

| Crate | Current doc-tests | Current ignored |
|-------|:-:|:-:|
| voom-domain | 0 | 0 |
| voom-dsl | 3 | 0 |
| voom-kernel | 0 | 0 |
| voom-plugin-sdk | 0 | 3 |
| voom-policy-evaluator | 0 | 0 |

## Design decisions

### Cross-crate imports allowed

Doc-tests may import sibling workspace crates freely (e.g. `voom_domain` types in `voom_kernel` doc-tests). These crates are organizational — no one consumes them independently.

### Doc-test patterns

**Builder chains** (voom-domain): Show `::new()` + `.with_*()` chain + assertion. Keep examples short — demonstrate the API shape, not every field.

**Pipeline examples** (voom-dsl, voom-policy-evaluator): Use a minimal `.voom` policy string as input, show the function call, assert on the output. Each function shows its piece of the parse → validate → compile → evaluate pipeline.

**Lifecycle examples** (voom-kernel): Create a minimal struct implementing `Plugin`, register it, dispatch an event, check the result.

**SDK round-trips** (voom-plugin-sdk): Construct a value, serialize, deserialize, assert equality.

### Handling the 3 ignored doc-tests in voom-plugin-sdk

- `load_plugin_config` (event.rs:35): Convert from `ignore` to a runnable test by providing a real closure that returns `Some(bytes)`.
- `load_plugin_config_named` (event.rs:47): Same treatment — provide a closure returning config bytes.
- `lib.rs` module-level example: Change from `ignore` to `no_run`. This example shows the WASM guest pattern using `wit_bindgen::generate!` which compiles but cannot execute in a host doc-test. Add a comment explaining why it's `no_run`.

## Crate-by-crate plan

### 1. voom-domain (~10 doc-tests)

| Item | Location | What to show |
|------|----------|-------------|
| `MediaFile::new` | media.rs | `new(path)` + `with_container` + `with_duration` + `with_tracks` chain |
| `Track::new` | media.rs | Construct video, audio, subtitle tracks |
| `TrackType::is_audio` / `is_subtitle` / `is_video` | media.rs | One example covering the category methods |
| `TrackType::from_str` | media.rs | Parse a string into a TrackType |
| `Container::from_extension` | media.rs | Map file extensions to Container variants |
| `Container::ffmpeg_format_name` | media.rs | Show which containers have ffmpeg format names |
| `Plan::new` | plan.rs | `new(file, policy, phase)` + `with_action` chain, assert `!is_empty()` |
| `PlannedAction::file_op` / `track_op` | plan.rs | Construct both variants |
| `TranscodeSettings` builder | plan.rs | Chain `with_crf`, `with_preset`, etc. |
| `Event::summary` | events.rs | Construct an event variant, call `summary()` |

### 2. voom-dsl (3 new doc-tests, 3 existing stay)

| Item | Location | What to show |
|------|----------|-------------|
| `parse_policy` | parser.rs | Parse minimal policy string, assert phase count |
| `validate` | validator.rs | Parse then validate, assert Ok |
| `format_policy` | formatter.rs | Parse, format, assert output contains expected text |

### 3. voom-kernel (3 doc-tests)

| Item | Location | What to show |
|------|----------|-------------|
| `Kernel::new` + `register_plugin` + `dispatch` | lib.rs | Define a minimal Plugin impl, register it, dispatch FileDiscovered, check results |
| `PluginContext::new` | lib.rs | Create context with config and data_dir |
| `PluginContext::parse_config` | lib.rs | Create context with JSON config, parse into a struct |

### 4. voom-plugin-sdk (4 doc-tests, fix 3 ignored)

| Item | Location | What to show |
|------|----------|-------------|
| `PluginInfoData::new` + builders | types.rs | Builder chain with capabilities |
| `serialize_event` / `deserialize_event` round-trip | event.rs | Serialize a FileDiscovered event, deserialize, assert equal |
| `load_plugin_config` | event.rs | Fix ignored test: provide closure returning TOML bytes, assert parsed config |
| `load_plugin_config_named` | event.rs | Fix ignored test: same pattern with named variant |
| Module-level example | lib.rs | Change `ignore` to `no_run` with explanatory comment |

### 5. voom-policy-evaluator (2 doc-tests)

| Item | Location | What to show |
|------|----------|-------------|
| `PolicyEvaluator::evaluate` | lib.rs | Compile a policy, create a MediaFile, evaluate, assert plan count |
| `evaluate_single_phase` | evaluator.rs | Compile policy, evaluate single phase, assert result |

## Acceptance criteria

- `cargo test --doc --workspace` reports >20 passing doc-tests
- 0 ignored doc-tests (the WASM example becomes `no_run`, not `ignore`)
- All doc-tests use `///` doc comments on the function they document
- No changes to non-documentation code (no refactoring, no new features)

## Out of scope

- Lower-priority crates (executors, storage, CLI, bus-tracer, etc.)
- Exhaustive coverage of every public method
- Refactoring code to improve doc-testability
