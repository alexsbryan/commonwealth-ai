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

# ── Workspace shape check ─────────────────────────────────────────────────
# Confirm the Cargo workspace is well-formed before we touch anything.
if ! cargo metadata --no-deps --format-version 1 >/dev/null 2>&1; then
    echo "✘ cargo metadata failed — workspace manifest is broken."
    echo "   Investigate before re-running this script."
    exit 1
fi
echo "✓ Cargo workspace resolves ($(cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c "import sys,json; print(len(json.loads(sys.stdin.read())['packages']))") members)."

# ── Daemon workspace pointer ──────────────────────────────────────────────
# `sovereign daemon run` reads this file to find the lint/test runner
# config. One-line text file at ~/.sovereign/workspace.
mkdir -p "${HOME}/.sovereign"
echo "$WORKSPACE_DIR" > "${HOME}/.sovereign/workspace"
echo "✓ Daemon workspace pointer wired: ${HOME}/.sovereign/workspace → ${WORKSPACE_DIR}"

# ── Adapter check ─────────────────────────────────────────────────────────
ADAPTER_DIR="${WORKSPACE_DIR}/sovereign/crates/sovereign-tools/src/code/test_adapters"
for adapter in sovereign-cargo-test-adapter sovereign-cargo-check-adapter; do
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

# ── Smoke ────────────────────────────────────────────────────────────────
echo
echo "Bootstrap complete. Smoke-test the regression gate:"
echo "  ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human --package commonwealth-api --filter auto_recover"
echo
echo "Definition-of-done before any feature push:"
echo "  ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human"
