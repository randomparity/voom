# VOOM development tasks

# List available recipes
default:
    @just --list

# By default, setup prompts before installing missing tools when stdin is a TTY.
# Set VOOM_SETUP_INSTALL=1 to install without prompts, or VOOM_SETUP_INSTALL=0
# to only check and report missing dependencies.

# First-time developer setup: check tools, install hooks, create venv
setup:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== VOOM Developer Setup ==="
    echo ""
    errors=0

    install_mode="${VOOM_SETUP_INSTALL:-prompt}"
    if [[ "$install_mode" != "0" && "$install_mode" != "1" && "$install_mode" != "prompt" ]]; then
        echo "✗ VOOM_SETUP_INSTALL must be 0, 1, or prompt"
        exit 1
    fi

    have() {
        command -v "$1" >/dev/null 2>&1
    }

    version_line() {
        "$@" 2>/dev/null | head -1 || true
    }

    package_manager() {
        if have brew; then
            echo brew
        elif have apt-get; then
            echo apt
        elif have dnf; then
            echo dnf
        elif have yum; then
            echo yum
        else
            echo none
        fi
    }

    package_names() {
        manager="$1"
        logical="$2"
        case "$manager:$logical" in
            brew:curl) echo "curl" ;;
            brew:git) echo "git" ;;
            brew:just) echo "just" ;;
            brew:python) echo "python@3.12" ;;
            brew:ffmpeg) echo "ffmpeg" ;;
            brew:mkvtoolnix) echo "mkvtoolnix" ;;
            brew:jq) echo "jq" ;;
            brew:sqlite3) echo "sqlite" ;;
            brew:mediainfo) echo "mediainfo" ;;
            brew:handbrake) echo "handbrake" ;;
            brew:espeak-ng) echo "espeak-ng" ;;
            brew:rclone) echo "rclone" ;;

            apt:curl) echo "curl" ;;
            apt:git) echo "git" ;;
            apt:just) echo "just" ;;
            apt:python) echo "python3 python3-venv" ;;
            apt:ffmpeg) echo "ffmpeg" ;;
            apt:mkvtoolnix) echo "mkvtoolnix" ;;
            apt:jq) echo "jq" ;;
            apt:sqlite3) echo "sqlite3" ;;
            apt:mediainfo) echo "mediainfo" ;;
            apt:handbrake) echo "handbrake-cli" ;;
            apt:espeak-ng) echo "espeak-ng" ;;
            apt:rclone) echo "rclone" ;;

            dnf:curl|yum:curl) echo "curl" ;;
            dnf:git|yum:git) echo "git" ;;
            dnf:just|yum:just) echo "just" ;;
            dnf:python|yum:python) echo "python3" ;;
            dnf:ffmpeg|yum:ffmpeg) echo "ffmpeg" ;;
            dnf:mkvtoolnix|yum:mkvtoolnix) echo "mkvtoolnix" ;;
            dnf:jq|yum:jq) echo "jq" ;;
            dnf:sqlite3|yum:sqlite3) echo "sqlite" ;;
            dnf:mediainfo|yum:mediainfo) echo "mediainfo" ;;
            dnf:handbrake|yum:handbrake) echo "HandBrake-cli" ;;
            dnf:espeak-ng|yum:espeak-ng) echo "espeak-ng" ;;
            dnf:rclone|yum:rclone) echo "rclone" ;;
            *) echo "" ;;
        esac
    }

    install_tip() {
        logical="$1"
        manager="$(package_manager)"
        packages="$(package_names "$manager" "$logical")"
        case "$manager" in
            brew)
                if [[ -n "$packages" ]]; then
                    echo "brew install $packages"
                else
                    echo "Install $logical with Homebrew or the vendor package."
                fi
                ;;
            apt)
                if [[ -n "$packages" ]]; then
                    echo "sudo apt-get update && sudo apt-get install -y --no-install-recommends $packages"
                else
                    echo "Install $logical with apt or the vendor package."
                fi
                ;;
            dnf)
                if [[ -n "$packages" ]]; then
                    echo "sudo dnf install -y $packages"
                else
                    echo "Install $logical with dnf or the vendor package."
                fi
                ;;
            yum)
                if [[ -n "$packages" ]]; then
                    echo "sudo yum install -y $packages"
                else
                    echo "Install $logical with yum or the vendor package."
                fi
                ;;
            *)
                echo "Install $logical with your system package manager."
                ;;
        esac
    }

    should_install() {
        logical="$1"
        if [[ "$install_mode" == "1" ]]; then
            return 0
        fi
        if [[ "$install_mode" == "0" || ! -t 0 ]]; then
            return 1
        fi
        read -rp "  Install $logical now? [y/N] " answer
        [[ "$answer" == "y" || "$answer" == "Y" ]]
    }

    install_system_package() {
        logical="$1"
        manager="$(package_manager)"
        packages="$(package_names "$manager" "$logical")"
        if [[ -z "$packages" || "$manager" == "none" ]]; then
            return 1
        fi

        case "$manager" in
            brew)
                brew install $packages
                ;;
            apt)
                sudo apt-get update
                sudo apt-get install -y --no-install-recommends $packages
                ;;
            dnf)
                sudo dnf install -y $packages
                ;;
            yum)
                sudo yum install -y $packages
                ;;
            *)
                return 1
                ;;
        esac
    }

    require_command() {
        command_name="$1"
        logical="$2"
        description="$3"
        if have "$command_name"; then
            echo "✓ $command_name ($description)"
            return 0
        fi

        echo "✗ $command_name not found — $description"
        echo "  Suggested install: $(install_tip "$logical")"
        if should_install "$command_name"; then
            if install_system_package "$logical" && have "$command_name"; then
                echo "✓ $command_name installed"
                return 0
            fi
            echo "  Automatic install did not provide $command_name."
        fi
        errors=$((errors + 1))
        return 1
    }

    optional_command() {
        command_name="$1"
        logical="$2"
        description="$3"
        if have "$command_name"; then
            echo "✓ $command_name ($description)"
            return 0
        fi

        echo "⚠ $command_name not found — $description"
        echo "  Suggested install: $(install_tip "$logical")"
        if should_install "$command_name"; then
            if install_system_package "$logical" && have "$command_name"; then
                echo "✓ $command_name installed"
            else
                echo "  Optional tool still unavailable: $command_name"
            fi
        fi
    }

    ensure_cargo_tool() {
        cargo_subcommand="$1"
        crate="$2"
        description="$3"
        if cargo "$cargo_subcommand" --version >/dev/null 2>&1; then
            echo "✓ cargo $cargo_subcommand ($description)"
            return 0
        fi

        echo "✗ cargo $cargo_subcommand not found — $description"
        echo "  Suggested install: cargo install --locked $crate"
        if should_install "cargo $cargo_subcommand"; then
            cargo install --locked "$crate"
            if cargo "$cargo_subcommand" --version >/dev/null 2>&1; then
                echo "✓ cargo $cargo_subcommand installed"
                return 0
            fi
        fi
        errors=$((errors + 1))
        return 1
    }

    echo "--- Core toolchain ---"
    if have rustup; then
        echo "✓ $(version_line rustup --version)"
    else
        echo "✗ rustup not found — install from https://rustup.rs"
        errors=$((errors + 1))
    fi

    if have cargo; then
        echo "✓ $(cargo --version)"
    else
        echo "✗ cargo not found — install Rust via https://rustup.rs"
        errors=$((errors + 1))
    fi

    require_command git git "required for hooks and source control" || true
    require_command curl curl "required for uv installation fallback and smoke tests" || true
    require_command python3 python "required for repository Python scripts and tests" || true
    require_command just just "required for repository task recipes" || true

    echo ""
    echo "--- Rust developer tools ---"
    if have cargo; then
        ensure_cargo_tool deny cargo-deny "runs just deny and CI dependency audits" || true
        ensure_cargo_tool insta cargo-insta "reviews snapshot updates" || true
        ensure_cargo_tool llvm-cov cargo-llvm-cov "generates SonarCloud-compatible coverage reports" || true
        ensure_cargo_tool mutants cargo-mutants "runs scheduled mutation testing locally" || true
        ensure_cargo_tool fuzz cargo-fuzz "runs DSL fuzz targets locally" || true
    fi

    echo ""
    echo "--- Media and script runtime tools ---"
    require_command ffmpeg ffmpeg "required for corpus generation, processing, verification, and integration tests" || true
    require_command ffprobe ffmpeg "required for media introspection and scan/process tests" || true
    require_command mkvmerge mkvtoolnix "required for MKV remux/container operations" || true
    require_command mkvpropedit mkvtoolnix "required for MKV metadata operations" || true
    require_command mkvextract mkvtoolnix "required for attachment/subtitle extraction workflows" || true
    require_command jq jq "required by policy audit scripts and JSON-based functional plans" || true
    require_command sqlite3 sqlite3 "required by e2e policy-audit database export scripts" || true
    optional_command mediainfo mediainfo "used by optional environment diagnostics" || true
    optional_command HandBrakeCLI handbrake "used by optional HandBrake executor workflows" || true
    optional_command rclone rclone "used by remote backup workflows when configured" || true
    optional_command espeak-ng espeak-ng "used for Linux speech test-corpus fixtures" || true

    echo ""
    echo "--- Python environment ---"
    if ! have uv; then
        echo "✗ uv not found — required to manage the repo Python environment"
        echo "  Suggested install: https://docs.astral.sh/uv/getting-started/installation/"
        if should_install uv; then
            if have brew; then
                brew install uv
            elif have curl; then
                curl -LsSf https://astral.sh/uv/install.sh | sh
                export PATH="$HOME/.local/bin:$PATH"
            fi
        fi
    fi

    if have uv; then
        echo "✓ $(uv --version)"
        if [[ "$install_mode" == "0" ]]; then
            if [[ -x .venv/bin/python ]]; then
                .venv/bin/python -c 'import importlib.util, pathlib, sys; sys.path.insert(0, str(pathlib.Path("wasm-plugins/tvdb-metadata/src").resolve())); missing = [name for name in ("pytest", "tvdb_metadata", "umsgpack") if importlib.util.find_spec(name) is None]; sys.exit(f"missing Python modules after setup: {chr(44).join(missing)}") if missing else None; sys.exit("Python 3.11+ is required for the tvdb-metadata plugin") if sys.version_info < (3, 11) else None'
                echo "✓ Python test imports verified"
            else
                echo "✗ .venv is missing — run just setup to create the Python dev environment"
                errors=$((errors + 1))
            fi
        else
            echo "Creating/updating .venv..."
            uv venv --quiet --allow-existing .venv
            uv pip install --quiet --python .venv/bin/python pre-commit pytest u-msgpack-python
            echo "✓ Python dev dependencies installed in .venv"

            .venv/bin/python -c 'import importlib.util, pathlib, sys; sys.path.insert(0, str(pathlib.Path("wasm-plugins/tvdb-metadata/src").resolve())); missing = [name for name in ("pytest", "tvdb_metadata", "umsgpack") if importlib.util.find_spec(name) is None]; sys.exit(f"missing Python modules after setup: {chr(44).join(missing)}") if missing else None; sys.exit("Python 3.11+ is required for the tvdb-metadata plugin") if sys.version_info < (3, 11) else None'
            echo "✓ Python test imports verified"

            echo "Installing git hooks..."
            if hooks_path=$(git config --get core.hooksPath 2>/dev/null); then
                echo "⚠ core.hooksPath is set to: $hooks_path"
                echo "  pre-commit cannot install hooks while this is set."
                echo "  Unset it for this repo with: git config --unset core.hooksPath"
                echo "  Then re-run: just setup"
            else
                .venv/bin/pre-commit install
                echo "✓ pre-commit hooks installed"
            fi
        fi
    else
        echo "✗ uv still unavailable; Python dependencies and hooks were not installed"
        errors=$((errors + 1))
    fi

    # --- Summary ---

    echo ""
    if [ "$errors" -gt 0 ]; then
        echo "Setup completed with $errors issue(s). See above."
        exit 1
    else
        echo "Setup complete! You're ready to develop."
    fi

# Check dependencies without attempting installation
setup-check:
    VOOM_SETUP_INSTALL=0 just setup

# Run repository Python tests via the setup-managed virtual environment
python-test:
    test -x .venv/bin/python || { echo "Missing .venv. Run: just setup"; exit 1; }
    .venv/bin/python -m pytest tests/scripts/test_generate_test_corpus.py wasm-plugins/tvdb-metadata/tests

# Build all workspace crates
build:
    cargo build --workspace

# Run all workspace tests
test:
    cargo test --workspace

# Run fixture-backed tests for shipped example policies
policy-test-examples:
    cargo run -q -- policy test docs/examples/tests

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
ci: fmt-check lint test policy-test-examples test-wasm functional-test-quick

# Run cargo-deny dependency audit
deny:
    cargo deny check

# Run the CLI
run *args:
    cargo run -- {{ args }}

# Review snapshot test changes
insta-review:
    cargo insta review

# Run functional tests (requires python3, ffmpeg, ffprobe on PATH)
functional-test:
    cargo test -p voom-cli --features functional -- --test-threads=4

# Run functional tests that don't need external media tools (fast)
functional-test-quick:
    cargo test -p voom-cli --features functional -- --test-threads=4 test_init test_doctor test_policy test_config test_status test_jobs test_plugin

# Clean build artifacts
clean:
    cargo clean
