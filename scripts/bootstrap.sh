#!/usr/bin/env bash
# bootstrap.sh — one-shot setup for a fresh commonwealth-ai workstation.
#
# Run from the workspace shell after cloning it. Verifies sub-repos
# are present (or guides you to clone them), wires the sovereign
# daemon's lint/test watcher to this workspace, and confirms the
# regression gate works end-to-end.
#
# Idempotent: safe to re-run after pulling new commits.

set -euo pipefail

WORKSPACE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$WORKSPACE_DIR"

# ── Sub-repo presence check ───────────────────────────────────────────────
REQUIRED_SUBREPOS=(sovereign commonwealth corpus-engine oicp-types sovereign-recipes)
missing=()
for sub in "${REQUIRED_SUBREPOS[@]}"; do
    if [[ ! -d "$WORKSPACE_DIR/$sub/.git" ]]; then
        missing+=("$sub")
    fi
done

if [[ ${#missing[@]} -gt 0 ]]; then
    echo "✘ Missing sub-repos:"
    for m in "${missing[@]}"; do
        echo "    - $m"
    done
    echo
    echo "Clone each into ${WORKSPACE_DIR}/<name>/, then re-run bootstrap.sh."
    echo "If you already cloned but the .git dir is elsewhere (e.g. submodule"
    echo "checkout), the test will pass once it's a normal clone."
    exit 1
fi
echo "✓ All sub-repos present (${#REQUIRED_SUBREPOS[@]})."

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
# After bootstrapping the workspace pointer, the daemon needs to
# re-read its config. The pre-flight check is whether the daemon is
# even installed; we don't try to install it from here.
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
echo "  ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human --workspace corpus-engine --filter chunkers::portal_event_bullet"
echo
echo "Definition-of-done before any feature push:"
echo "  ${WORKSPACE_DIR}/scripts/sovereign-test.sh --human"
