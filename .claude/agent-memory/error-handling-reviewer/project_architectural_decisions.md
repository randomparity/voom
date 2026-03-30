---
name: VOOM error handling architectural decisions
description: Authoritative decisions about where String errors are intentional vs. targets for improvement
type: project
---

## Intentional String errors (WIT/WASM boundary)

`crates/voom-kernel/src/host/store.rs` — `WasmPluginStore` trait and `StorageBackedPluginStore` use `Result<T, String>` by design. WIT interfaces can only carry string errors across the WASM ABI. The doc comment says so explicitly. `map_err(|e| e.to_string())` here is correct and should not be "fixed".

`crates/voom-kernel/src/loader.rs:546` — same reason; linker function wrappers at the WASM boundary use `Result<_, String>`.

## String errors that are legitimately a smell

`crates/voom-cli/src/commands/process.rs:276,288` — `process_single_file` returns `Result<_, String>` because the worker pool processor closure requires that signature. This is a leaky abstraction: the worker pool forces `String` errors all the way from typed `VoomError`/`anyhow::Error` sources. A future improvement would be to make `WorkerPool::process_batch` generic over the error type, or to accept `Box<dyn Error>`.

## Infallible orchestration (design decision)

`orchestrate_plans` returns `OrchestrationResult` (not `Result`). This is correct: phase orchestration is pure logic over already-validated data and cannot fail. The old `Result` was defensive code with no actual error paths.

## #[non_exhaustive] strategy

Both `VoomError` and `StorageErrorKind` are `#[non_exhaustive]`. This means plugin code and downstream crates that match on these enums must include a wildcard arm. This is correct for a growing library — prevents the enum from being a sealed set of variants. The tradeoff is that `match` exhaustiveness is not compiler-enforced from external crates.
