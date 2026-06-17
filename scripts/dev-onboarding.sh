#!/usr/bin/env bash
# dev-onboarding.sh — replay the desktop onboarding SCREENS as if this
# were a fresh install, without wiping ~/.sovereign, without re-downloading
# models, and without threading env vars by hand.
#
# Sets the two in-memory override flags and launches the app:
#   SOVEREIGN_DEV_FORCE_SETUP=1      replay the setup wizard
#                                    (WelcomeThreshold -> SetupFlow). Model
#                                    steps short-circuit on existing GGUFs,
#                                    so they play through fast.
#   SOVEREIGN_DEV_FORCE_FIRST_RUN=1  replay the recipe-author onboarding
#                                    (is_first_run -> true, empty project
#                                    list -> the first-timer tutorial CTA).
#
# Both flags are IN-MEMORY ONLY (see src-tauri/src/dev_flags.rs): your real
# config, projects, corpora and models are untouched and reappear on the
# next plain launch.
#
# Scope: this replays the SCREENS against your real data, so chat is NOT
# empty. Auditing a truly empty backend (empty chat, real first-run
# download) is intentionally out of scope — it requires a real model
# download and is not worth the machinery; just inspect the screens here.
#
# WHY `cargo tauri dev`: a debug Tauri build loads its UI from the Vite dev
# server (devUrl = http://localhost:5173), not the embedded bundle. This
# starts Vite (beforeDevCommand) + the app together and gives you HMR.
#
# Usage:
#   scripts/dev-onboarding.sh                 # replay both gates
#   scripts/dev-onboarding.sh --setup-only    # replay Gate 1 only
#   scripts/dev-onboarding.sh -- <args...>    # pass extra args to the app
set -euo pipefail

# Repo root is derived from this script's own location — no hardcoded paths.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP_CRATE="$REPO_ROOT/sovereign/crates/sovereign-desktop"

FORCE_FIRST_RUN=1
PASSTHRU=()
while [ $# -gt 0 ]; do
  case "$1" in
    --setup-only) FORCE_FIRST_RUN=0; shift ;;
    --)           shift; PASSTHRU=("$@"); break ;;
    *)            PASSTHRU+=("$1"); shift ;;
  esac
done

# A stale orphaned dev sovereign-server holds :8080 and crash-loops the
# desktop's mobile host. Clear any dev-build instance (safe — only touches
# target/debug binaries; the desktop spawns its own).
if pgrep -f "target/debug/sovereign-server" >/dev/null 2>&1; then
  echo "[dev-onboarding] clearing a stale target/debug/sovereign-server (frees :8080)"
  pkill -9 -f "target/debug/sovereign-server" || true
fi

ENV_ARGS=(SOVEREIGN_DEV_FORCE_SETUP=1)
if [ "$FORCE_FIRST_RUN" -eq 1 ]; then
  ENV_ARGS+=(SOVEREIGN_DEV_FORCE_FIRST_RUN=1)
fi

echo "[dev-onboarding] starting Vite + desktop via 'cargo tauri dev'; in-memory overrides:"
for e in "${ENV_ARGS[@]}"; do echo "                 $e"; done

# Run from the crate dir so the tauri CLI finds src-tauri/. exec so Ctrl-C
# reaches the CLI (which tears down Vite + the app together). App args, if
# any, go after a literal "-- --". The "${arr[@]+...}" guard keeps an empty
# PASSTHRU from tripping `set -u` under macOS's bash 3.2.
cd "$DESKTOP_CRATE"
if [ "${#PASSTHRU[@]}" -gt 0 ]; then
  exec env "${ENV_ARGS[@]}" cargo tauri dev -- -- ${PASSTHRU[@]+"${PASSTHRU[@]}"}
else
  exec env "${ENV_ARGS[@]}" cargo tauri dev
fi
