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
