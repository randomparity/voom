# VOOM development tasks

# List available recipes
default:
    @just --list

# First-time developer setup: check tools, install hooks, create venv
setup:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== VOOM Developer Setup ==="
    echo ""
    errors=0

    # --- Required tools ---

    # Rust toolchain
    if command -v rustup &>/dev/null; then
        echo "✓ rustup $(rustup --version 2>/dev/null | head -1 | awk '{print $2}')"
    else
        echo "✗ rustup not found — install from https://rustup.rs"
        errors=$((errors + 1))
    fi

    if command -v cargo &>/dev/null; then
        echo "✓ cargo $(cargo --version | awk '{print $2}')"
    else
        echo "✗ cargo not found — install Rust via https://rustup.rs"
        errors=$((errors + 1))
    fi

    # just (already running, but check anyway)
    if command -v just &>/dev/null; then
        echo "✓ just $(just --version | awk '{print $2}')"
    else
        echo "✗ just not found — install from https://github.com/casey/just"
        errors=$((errors + 1))
    fi

    # --- Optional tools ---

    if command -v cargo-deny &>/dev/null || cargo deny --version &>/dev/null 2>&1; then
        echo "✓ cargo-deny $(cargo deny --version 2>/dev/null | awk '{print $2}')"
    else
        echo "⚠ cargo-deny not found (optional) — install with: cargo install cargo-deny"
    fi

    if command -v cargo-insta &>/dev/null || cargo insta --version &>/dev/null 2>&1; then
        echo "✓ cargo-insta $(cargo insta --version 2>/dev/null | awk '{print $2}')"
    else
        echo "⚠ cargo-insta not found (optional) — install with: cargo install cargo-insta"
    fi

    # --- Python / uv (for pre-commit) ---

    echo ""
    if command -v uv &>/dev/null; then
        echo "✓ uv $(uv --version | awk '{print $2}')"
    else
        echo "✗ uv not found"
        echo "  uv is needed to manage the pre-commit Python environment."
        echo "  Install it? See: https://docs.astral.sh/uv/getting-started/installation/"
        echo ""
        read -rp "  Install uv now via the official installer? [y/N] " answer
        if [[ "${answer,,}" == "y" ]]; then
            curl -LsSf https://astral.sh/uv/install.sh | sh
            export PATH="$HOME/.local/bin:$PATH"
            echo "✓ uv installed ($(uv --version | awk '{print $2}'))"
        else
            echo "  Skipping uv install. Pre-commit hooks will not be set up."
            errors=$((errors + 1))
        fi
    fi

    # --- Set up pre-commit via uv ---

    if command -v uv &>/dev/null; then
        echo ""
        echo "Setting up Python venv and pre-commit..."
        uv venv --quiet .venv
        uv pip install --quiet pre-commit
        echo "✓ pre-commit installed in .venv"

        echo "Installing git hooks..."
        if hooks_path=$(git config --get core.hooksPath 2>/dev/null); then
            echo "⚠ core.hooksPath is set to: $hooks_path"
            echo "  pre-commit cannot install hooks while this is set."
            echo "  Options:"
            echo "    1) Unset it for this repo:  git config --unset core.hooksPath"
            echo "    2) Unset it globally:       git config --global --unset core.hooksPath"
            echo "  Then re-run: just setup"
            echo ""
            echo "  Skipping hook installation."
        else
            .venv/bin/pre-commit install
            echo "✓ pre-commit hooks installed"
        fi
    fi

    # --- Summary ---

    echo ""
    if [ "$errors" -gt 0 ]; then
        echo "Setup completed with $errors issue(s). See above."
        exit 1
    else
        echo "Setup complete! You're ready to develop."
    fi

# Build all workspace crates
build:
    cargo build --workspace

# Run all workspace tests
test:
    cargo test --workspace

# Run tests for a single crate (e.g., just test-crate voom-dsl)
test-crate crate:
    cargo test -p {{ crate }}

# Run WASM-feature-gated kernel tests
test-wasm:
    cargo test -p voom-kernel --features wasm

# Lint with clippy
lint:
    cargo clippy --workspace -- -D warnings
    cargo clippy -p voom-kernel --features wasm -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run the full CI check locally (fmt + clippy + tests)
ci: fmt-check lint test test-wasm

# Run cargo-deny dependency audit
deny:
    cargo deny check

# Run the CLI
run *args:
    cargo run -- {{ args }}

# Review snapshot test changes
insta-review:
    cargo insta review

# Clean build artifacts
clean:
    cargo clean
