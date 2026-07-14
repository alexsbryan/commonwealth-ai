#!/usr/bin/env bash
# bootstrap-mac.sh — one-shot installer for the macOS build deps.
#
# The macOS parallel to bootstrap-linux.sh. Gets a fresh Mac from a
# `git clone` to `cargo build`-ready:
#   1. Xcode command-line tools (clang + the macOS SDK).
#   2. Homebrew packages: lld (the workspace links with it), protobuf, cmake.
#   3. Rust via rustup, plus the rustfmt + rust-analyzer components.
#   4. A persisted SDKROOT so llama-cpp-sys-4's bindgen finds the system
#      headers (without it the build dies with `'memory' file not found`).
#
# Apple Silicon is the tested target — the Metal backend is cfg-gated and
# committed in crates/sovereign-inference/Cargo.toml, so there is no
# Cargo.toml edit and git stays clean. Intel Macs build too, but aren't
# exercised in CI (the macos-13 Intel runner was retired).
#
# Idempotent — safe to re-run after a pull.
#
# Usage:
#   ./scripts/bootstrap-mac.sh              # full setup
#   ./scripts/bootstrap-mac.sh --no-brew    # skip `brew install` (deps already present)
#   ./scripts/bootstrap-mac.sh --help

set -euo pipefail

NO_BREW=0

die()  { echo "bootstrap-mac: $*" >&2; exit 1; }
warn() { echo "bootstrap-mac: warning: $*" >&2; }
note() { echo "== $* =="; }

parse_args() {
    for arg in "$@"; do
        case "$arg" in
            --no-brew) NO_BREW=1 ;;
            -h|--help)
                sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;/^set -/d'
                exit 0 ;;
            *) die "unknown flag: $arg (see --help)" ;;
        esac
    done
}

require_macos() {
    [[ "$(uname -s)" == "Darwin" ]] || die "this is the macOS bootstrap — on Linux use bootstrap-linux.sh"
}

# Xcode command-line tools ship clang + the macOS SDK that llama-cpp-sys
# builds its C++ bindings against. `xcode-select --install` opens a GUI
# installer we can't drive to completion from a script, so detect + instruct
# rather than fail cryptically five minutes into the first build.
ensure_xcode_clt() {
    if xcode-select -p >/dev/null 2>&1; then
        return 0
    fi
    note "Installing Xcode command-line tools"
    xcode-select --install 2>/dev/null || true
    cat >&2 <<'EOF'

A GUI installer for the Xcode command-line tools just opened (or was
already running). Finish it, then re-run this script — the rest of setup
needs the compiler + SDK it installs.

EOF
    die "waiting on Xcode command-line tools"
}

ensure_homebrew() {
    if command -v brew >/dev/null 2>&1; then
        return 0
    fi
    cat >&2 <<'EOF'

Homebrew isn't installed, and it's how this script gets lld / protobuf /
cmake. Install it with the official one-liner, then re-run this script:

  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

EOF
    die "Homebrew required"
}

install_brew_deps() {
    note "Installing build deps via Homebrew (lld, protobuf, cmake)"
    # lld    : the workspace .cargo/config.toml links macOS with `-fuse-ld=lld`;
    #          clang can't find it if the `lld` formula isn't installed.
    # protobuf: prost-build / tonic (the mesh gRPC) need `protoc` at build time.
    # cmake  : llama-cpp-sys-4 drives llama.cpp's CMake build.
    brew install lld protobuf cmake
}

ensure_rust() {
    if ! (command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1); then
        note "Installing Rust via rustup"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
        # shellcheck source=/dev/null
        . "$HOME/.cargo/env"
    fi
    # rustfmt: llama-cpp-sys-4's bindgen pipes its generated bindings through
    #   it; rustup's minimal profile omits it.
    # rust-analyzer: the code-intel daemon shells out to `rust-analyzer scip`
    #   to build the SCIP call graph behind `symbols` / `callers` /
    #   `sovereign project refresh`. `rust-analyzer` on PATH is a rustup proxy
    #   shim — if the component isn't installed for the pinned toolchain the
    #   export fails, AND a failed export WIPES the graph to zero. Install it
    #   up front. Run from the repo so rustup targets the pinned toolchain.
    if ! command -v rustfmt >/dev/null 2>&1; then
        note "Installing rustfmt component"
        rustup component add rustfmt
    fi
    if ! rustup component list --installed 2>/dev/null | grep -q '^rust-analyzer'; then
        note "Installing rust-analyzer component"
        rustup component add rust-analyzer
    fi
}

# llama-cpp-sys-4's bindgen resolves system headers through the macOS SDK and
# fails with `'memory' file not found` unless SDKROOT points at it. It's needed
# at every `cargo build`, so persist it to the login shell's rc as well as
# exporting it for this process. Mirrors the /etc/profile.d drop the Linux
# bootstrap writes for ROCm.
persist_sdkroot() {
    local sdk
    sdk="$(xcrun --show-sdk-path 2>/dev/null)" || die "xcrun couldn't find the SDK — are the Xcode CLT installed?"
    export SDKROOT="$sdk"

    local rc
    case "$(basename "${SHELL:-/bin/zsh}")" in
        bash) rc="$HOME/.bash_profile" ;;          # macOS bash reads this for login shells
        *)    rc="$HOME/.zshrc" ;;                  # zsh is the macOS default since Catalina
    esac

    local marker="# commonwealth-ai: SDKROOT for llama-cpp-sys bindgen"
    if [[ -f "$rc" ]] && grep -qF "$marker" "$rc"; then
        note "SDKROOT already wired in $rc"
        return 0
    fi
    note "Persisting SDKROOT to $rc"
    {
        echo ""
        echo "$marker (added by sovereign/scripts/bootstrap-mac.sh — safe to delete)"
        echo 'export SDKROOT="$(xcrun --show-sdk-path)"'
    } >> "$rc"
}

main() {
    parse_args "$@"
    require_macos
    ensure_xcode_clt

    if (( NO_BREW )); then
        note "--no-brew: skipping Homebrew install (deps assumed present)"
    else
        ensure_homebrew
        install_brew_deps
    fi

    ensure_rust
    persist_sdkroot

    cat <<EOF

== Done ==

Backend: Metal (cfg-gated + committed — no Cargo.toml edit, git stays clean).
SDKROOT is set for this shell and persisted for new ones.

From the repo root, build the CLI:

  cargo build --release -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm

Then wire the daemon's lint/test watcher:

  ./scripts/bootstrap.sh

EOF
}

main "$@"
