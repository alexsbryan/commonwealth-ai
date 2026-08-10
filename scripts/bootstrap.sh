#!/usr/bin/env bash
# bootstrap.sh — one-shot setup for a fresh commonwealth-ai workstation.
#
# Post-monorepo (2026-05-10) this is dramatically simpler than the
# pre-merge multi-repo version: one git clone gets the whole tree, so
# this script's job is just wiring the daemon's lint/test watcher to
# the workspace and confirming the regression gate is green.
#
# Idempotent — safe to re-run after pulls.

set -euo pipefail

WORKSPACE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_DIR"

# Per-user root resolution (binary-first, legacy-aware). Must be sourced
# before anything creates a directory under $HOME.
# shellcheck source=scripts/lib/svrn-root.sh
. "${WORKSPACE_DIR}/scripts/lib/svrn-root.sh"

# ── Workspace shape check ─────────────────────────────────────────────────
# Confirm the Cargo workspace is well-formed before we touch anything.
if ! cargo metadata --no-deps --format-version 1 >/dev/null 2>&1; then
    echo "✘ cargo metadata failed — workspace manifest is broken."
    echo "   Investigate before re-running this script."
    exit 1
fi
echo "✓ Cargo workspace resolves ($(cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c "import sys,json; print(len(json.loads(sys.stdin.read())['packages']))") members)."

# ── Test-runner tooling: cargo-nextest ────────────────────────────────────
# scripts/nextest.sh is the fast-path dev runner. Plain `cargo test` executes
# the workspace's ~229 test binaries SERIALLY: measured 2026-07-24 at 90.5s of
# in-binary time against a 16.7s slowest binary, so nextest's cross-binary
# parallelism takes a warm full run from ~126s to roughly 42s. (It saves only a
# few percent on a COLD build, where compilation dominates — nextest is an
# iteration-speed win, not a cold-build one.)
#
# PINNED so every machine on the mesh runs the same runner: profiles and
# per-test overrides live in the repo's .config/nextest.toml, and a version
# skew there is a silent behaviour skew across the fleet. Bump deliberately.
NEXTEST_VERSION="0.9.140"

# `cargo nextest --version` prints a five-line block (banner + release/commit/
# host detail), so take the banner line only — parsing the whole block yields
# a multi-value string that never equals the pin, and bootstrap reinstalls on
# every run.
installed_nextest="$(cargo nextest --version 2>/dev/null | head -1 | awk '{print $2}')"
if [[ "$installed_nextest" == "$NEXTEST_VERSION" ]]; then
    echo "✓ cargo-nextest ${NEXTEST_VERSION} present."
else
    if [[ -n "$installed_nextest" ]]; then
        echo "cargo-nextest ${installed_nextest} present but fleet pin is ${NEXTEST_VERSION} — reinstalling..."
    else
        echo "Installing cargo-nextest ${NEXTEST_VERSION} (a few minutes; builds from crates.io)..."
    fi
    # Non-fatal: nextest is the fast path, not the gate. sovereign-test.sh is
    # the definition-of-done runner and needs nothing beyond cargo, so a failed
    # install must not abort bootstrap on a machine that can still test.
    if cargo install cargo-nextest --locked --version "$NEXTEST_VERSION" 2>&1 | tail -3; then
        echo "✓ cargo-nextest ${NEXTEST_VERSION} installed."
    else
        echo "⚠  cargo-nextest install failed — skipping."
        echo "   scripts/nextest.sh will be unavailable; scripts/sovereign-test.sh still works."
        echo "   Retry: cargo install cargo-nextest --locked --version ${NEXTEST_VERSION}"
    fi
fi

# ── Daemon workspace pointer ──────────────────────────────────────────────
# `sovereign daemon run` reads this file to find the lint/test runner
# config. One-line text file at <root>/workspace.
#
# The root is RESOLVED, never hard-coded: on a machine that still has a
# populated ~/.sovereign and no ~/.svrnmesh, creating ~/.svrnmesh here
# would make the Rust getters prefer it and orphan the real data root.
# See scripts/lib/svrn-root.sh.
SVRN_ROOT="$(svrn_root)"
mkdir -p "${SVRN_ROOT}"
echo "$WORKSPACE_DIR" > "${SVRN_ROOT}/workspace"
echo "✓ Daemon workspace pointer wired: ${SVRN_ROOT}/workspace → ${WORKSPACE_DIR}"

# ── Adapter check ─────────────────────────────────────────────────────────
ADAPTER_DIR="${WORKSPACE_DIR}/sovereign/crates/sovereign-tools/src/code/test_adapters"
for adapter in sovereign-cargo-test-adapter sovereign-cargo-check-adapter \
               sovereign-nextest-junit-adapter; do
    if [[ ! -x "${ADAPTER_DIR}/${adapter}" ]]; then
        echo "⚠  Adapter not executable: ${adapter} — running chmod +x"
        chmod +x "${ADAPTER_DIR}/${adapter}" 2>/dev/null || {
            echo "  (couldn't chmod — adapter may have wrong permissions on this fs)"
        }
    fi
done
echo "✓ Test/lint adapters executable."

# ── Daemon restart (macOS launchd / Linux systemd) ────────────────────────
case "$(uname -s)" in
    Darwin)
        if launchctl list 2>/dev/null | grep -q com.sovereign.daemon; then
            echo "Restarting com.sovereign.daemon to pick up workspace pointer..."
            launchctl bootout "gui/$(id -u)/com.sovereign.daemon" 2>/dev/null || true
            launchctl bootstrap "gui/$(id -u)" "${HOME}/Library/LaunchAgents/com.sovereign.daemon.plist" 2>/dev/null || {
                echo "  (couldn't restart — run manually:"
                echo "    launchctl bootout gui/$(id -u)/com.sovereign.daemon"
                echo "    launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.sovereign.daemon.plist )"
            }
            echo "✓ Daemon restarted."
        else
            echo "ℹ  Sovereign daemon not registered with launchd — the lint/test"
            echo "   watcher won't run until you set it up. The scripts still"
            echo "   work standalone via:"
            echo "     ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human"
        fi
        ;;
    Linux)
        if systemctl --user list-unit-files 2>/dev/null | grep -q sovereign.service; then
            echo "Restarting sovereign.service..."
            systemctl --user restart sovereign.service || {
                echo "  (couldn't restart — run manually: systemctl --user restart sovereign)"
            }
            echo "✓ Daemon restarted."
        else
            echo "ℹ  Sovereign daemon not registered with systemd-user — the"
            echo "   lint/test watcher won't run until you set it up. The"
            echo "   scripts still work standalone via:"
            echo "     ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human"
        fi
        ;;
    *)
        echo "ℹ  Unknown OS — skipping daemon restart."
        ;;
esac

# ── Git hooks ────────────────────────────────────────────────────────────
# The pre-push hook is this repo's PRIMARY correctness gate — CI is the
# confirmation pass, not the thing that stops bad code (docs/CI_ECONOMY.md
# explains why: a metered gate is a gate you eventually ration, and on
# 2026-07-24 the Actions allowance ran out and every check silently stopped
# running). Installing it is therefore part of bootstrap, not an optional
# extra. Non-fatal: a failure here must not break a machine's setup.
echo
if ! "${WORKSPACE_DIR}/scripts/install-git-hooks.sh"; then
    echo "⚠  Git hook install failed — run scripts/install-git-hooks.sh by hand."
fi

# ── Smoke ────────────────────────────────────────────────────────────────
echo
echo "Bootstrap complete. Smoke-test the regression gate:"
echo "  ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human --package commonwealth-api --filter auto_recover"
echo
echo "Definition-of-done before any feature push:"
echo "  ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human"
