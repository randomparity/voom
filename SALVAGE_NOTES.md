fdca66a Distinguish malformed plugin config -- skipped: conflicts across SDK plus multiple WASM plugin implementations; manual reimplementation candidate.
bb85060 Reject malformed WASM event results -- skipped: depends on branch-only WASM loader split/wit_runtime.rs; manual reimplementation candidate on current loader.
fb786d1 Preserve plugin data read errors -- skipped: conflicts across plugin SDK and many WASM plugin implementations; manual reimplementation candidate after SDK shape is reviewed.
def5cb8 Propagate host list entry errors -- skipped: current main lacks the affected list_files host API, so cherry-pick would introduce broader host surface instead of a narrow fix.
945f0d1 Propagate transition record errors -- skipped: conflicts depend on branch-only process workflow module reshuffle; manual narrow reimplementation candidate.
5e59cc6 Future-proof public domain DTOs -- skipped: broad conflicts across process, WIT conversion, ffmpeg, sqlite, and event handlers; not salvageable as a focused PR slice.
841499d Use named domain constructor inputs -- skipped: depends on branch SQLite module layout and touches broad verifier/storage constructor shapes.
