#!/usr/bin/env bash
# Build the test WASM plugin used by
# `crates/voom-kernel/tests/wasm_streaming_dispatch.rs`. Targets
# `wasm32-wasip2`, which produces a component-model artifact natively
# (no `cargo-component` / `wasm-tools` required).
set -euo pipefail

cd "$(dirname "$0")"

if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip2$'; then
    echo "wasm32-wasip2 target is not installed. Install it with:"
    echo "  rustup target add wasm32-wasip2"
    exit 1
fi

echo "Building wasm-streaming-test-plugin for wasm32-wasip2..."
# WIT_REQUIRE_F32_F64=0: voom-wit's types.wit still uses the legacy `float64`
# spelling, which wit-bindgen 0.51's parser deprecates. wasmtime 44 (the host
# runtime) still accepts it. This env var keeps wit-bindgen accepting it
# until the WIT files are migrated to `f32` / `f64`.
WIT_REQUIRE_F32_F64=0 cargo build --release --target wasm32-wasip2

WASM_PATH="target/wasm32-wasip2/release/wasm_streaming_test_plugin.wasm"
if [ ! -f "$WASM_PATH" ]; then
    echo "Error: expected $WASM_PATH was not produced. Check the build output above."
    exit 1
fi

# The kernel's WASM loader auto-discovers the plugin manifest by reading the
# `.toml` file next to the `.wasm` (with the same stem). The fixture stores
# the manifest at the project root under a hyphenated name, so copy it into
# the output directory with the underscored stem the kernel expects.
SRC_MANIFEST="wasm-streaming-test-plugin.toml"
DST_MANIFEST="target/wasm32-wasip2/release/wasm_streaming_test_plugin.toml"
cp "$SRC_MANIFEST" "$DST_MANIFEST"

echo "Built: $(cd "$(dirname "$WASM_PATH")" && pwd)/$(basename "$WASM_PATH")"
echo "Manifest copied to: $(cd "$(dirname "$DST_MANIFEST")" && pwd)/$(basename "$DST_MANIFEST")"
echo ""
echo "Run the integration tests with:"
echo "  cargo test -p voom-kernel --test wasm_streaming_dispatch --features wasm"
