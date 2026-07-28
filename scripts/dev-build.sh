#!/usr/bin/env bash
# dev-build.sh — the canonical debug build for this workspace.
#
# One command, feature soup included, so "I rebuilt but the verb/tool is
# gone" and "cargo rebuilt 17 crates twice" stop happening:
#
#   corpus-engine/treesitter   keeps feature unification aligned with what
#       the watcher, scripts, and CI resolve. A bare `cargo build` (or a
#       narrow `-p corpus-engine`) resolves treesitter OFF and forces
#       corpus-engine + ~17 dependents to rebuild — twice, once more on
#       the next full build (~80s/flip measured 2026-07-02).
#   sovereign-cli/dev-tools    keeps the gated dev verbs (`project`, …) in
#       the sovereign-cli dispatcher. A debug build without it silently
#       loses the verb (observed 2026-07-23).
#
# The deployed toolchain on dev boxes is target/debug — the
# ~/.local/bin/sovereign symlink and the daemon both point there — so this
# IS the deploy build. After it finishes, `sovereign daemon restart` loads
# the new sovereign-cli-daemon; other verbs pick up new siblings on next
# invocation. (Release deploys are a different path: scripts/dev-release.sh.)
#
# Usage:
#   scripts/dev-build.sh                      # full workspace, debug
#   scripts/dev-build.sh -p sovereign-cli-dev # one crate, same features
#   scripts/dev-build.sh --release            # forwarded verbatim

set -euo pipefail
cd "$(dirname "$0")/.."

# ── Preflight: is there a C toolchain at all? ──────────────────────────────
#
# `llama-cpp-sys-4` compiles llama.cpp and runs bindgen, both of which need
# clang. On a bare Fedora HOST there is none, so cargo dies with
#
#   error: failed to run custom build command for `llama-cpp-sys-4 v0.4.2`
#
# followed by a build-script backtrace whose real cause ("linker `clang` not
# found", or bindgen's `'stdbool.h' file not found`) is buried several
# screens down. That reads like the workspace is broken when the only thing
# wrong is WHERE you ran it — the same confusion that cost a session on
# 2026-07-28 via scripts/sovereign-lint.sh. Fail in one line instead, and
# name the fix. Measured: host exit 101, toolbox exit 0.
if ! command -v clang >/dev/null 2>&1; then
    in_container=""
    [[ -f /run/.containerenv || -f /.dockerenv ]] && in_container=1
    {
        echo "dev-build: no C toolchain — clang is not on PATH."
        echo
        if [[ -n "$in_container" ]]; then
            echo "  You are inside a container that lacks clang. llama-cpp-sys-4"
            echo "  cannot build here. Use the sovereign-vulkan toolbox:"
        else
            echo "  You are on the host. llama-cpp-sys-4 cannot build here."
            echo "  Build inside the toolbox:"
        fi
        echo
        echo "    toolbox run -c sovereign-vulkan ./scripts/dev-build.sh $*"
        echo
        echo "  The same applies to scripts/sovereign-lint.sh and"
        echo "  scripts/sovereign-test.sh."
    } >&2
    exit 2
fi

FEATURES="corpus-engine/treesitter,sovereign-cli/dev-tools"

# --workspace conflicts with an explicit -p/--package selection; drop it
# when the caller narrows, keep the feature soup either way.
scope=(--workspace)
for arg in "$@"; do
    case "$arg" in
        -p|--package|-p*|--package=*) scope=() ;;
    esac
done

echo "→ cargo build ${scope[*]:-} --features $FEATURES $*" >&2
exec cargo build ${scope[@]+"${scope[@]}"} --features "$FEATURES" "$@"
