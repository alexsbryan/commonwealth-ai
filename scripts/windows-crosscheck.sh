#!/usr/bin/env bash
# windows-crosscheck.sh — local Windows (x86_64-pc-windows-msvc) compile gate.
#
# Cross-compiles the desktop's Rust/cfg layer to Windows DIRECTLY ON macOS via
# cargo-xwin (clang-cl + lld-link + an auto-downloaded MSVC CRT/SDK). NO Windows
# machine and NO container (Docker/Podman) needed. Run this BEFORE pushing a
# Windows release tag so compile errors are caught LOCALLY (free) instead of on
# a CI runner (paid). Iterating against it costs zero Actions minutes.
#
# What it proves / what it doesn't:
#   - PROVES: every cfg-gating / unix-only / Rust compile error across the
#     desktop's Windows dependency tree. That layer is toolchain-independent, so
#     green here ⇒ it compiles on a real windows-msvc toolchain too.
#   - DOES NOT prove: the llama.cpp / onnxruntime NATIVE build (cmake → MSVC).
#     cargo-xwin drives clang-cl, not MSVC cl.exe; if it stalls inside the
#     llama-cpp-sys-4 / ort-sys native build, that is a cargo-xwin limitation,
#     NOT a real-CI signal — do not chase it. Prove the native build on a
#     Windows VM or one prepared CI run (see sovereign-desktop/RELEASING.md).
#
# Usage:
#   scripts/windows-crosscheck.sh                          # cargo xwin check (fast)
#   scripts/windows-crosscheck.sh --build                  # cargo xwin build (full)
#   scripts/windows-crosscheck.sh --features windows-vulkan  # a GPU backend variant
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

# Separate, ABSOLUTE target dir so the windows-msvc build neither stomps the
# host target/ (watcher cargo-lock contention) nor lands inside the crate dir.
# Absolute is load-bearing: this script cd's into the crate before invoking
# cargo, so a RELATIVE CARGO_TARGET_DIR would resolve there and leak ~11k build
# files into source control. Gitignored as `target-xwin/`.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target-xwin}"

log() { printf '\n[windows-crosscheck] %s\n' "$*"; }

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "windows-crosscheck: written for macOS hosts (cargo-xwin)." >&2
    exit 2
fi

TARGET="x86_64-pc-windows-msvc"
CARGO_VERB="check"
PASSTHRU=()
for arg in "$@"; do
    case "$arg" in
        --build) CARGO_VERB="build" ;;
        --check) CARGO_VERB="check" ;;
        *)       PASSTHRU+=("$arg") ;;
    esac
done

# ─── Prerequisites ────────────────────────────────────────────────────
command -v rustup >/dev/null || { echo "rustup is required." >&2; exit 2; }
if ! rustup target list --installed | grep -qx "$TARGET"; then
    log "Adding rust target $TARGET"
    rustup target add "$TARGET"
fi

command -v brew >/dev/null 2>&1 || {
    echo "Homebrew is required (provides llvm's clang-cl + lld-link)." >&2; exit 2; }

# clang-cl + lld-link come from the keg-only `llvm` formula — put it on PATH.
LLVM_BIN="$(brew --prefix llvm 2>/dev/null)/bin"
if [[ ! -x "$LLVM_BIN/clang-cl" ]]; then
    log "Installing llvm (clang-cl/lld-link) via brew…"
    brew install llvm
fi
export PATH="$LLVM_BIN:$PATH"
command -v clang-cl >/dev/null || { echo "clang-cl missing after llvm install." >&2; exit 2; }

if ! cargo xwin --version >/dev/null 2>&1; then
    log "Installing cargo-xwin…"
    cargo install --locked cargo-xwin
fi

# cargo-xwin downloads the MSVC CRT + Windows SDK headers on first run; accept
# the Microsoft license non-interactively (redistributable headers/libs only).
export XWIN_ACCEPT_LICENSE=1

# ─── Cross-check ──────────────────────────────────────────────────────
log "Tools: $(clang-cl --version | head -1) | $(cargo xwin --version)"
log "Running: cargo xwin $CARGO_VERB --target $TARGET ${PASSTHRU[*]:-} (in sovereign-desktop)"
log "Reminder: a failure INSIDE the llama.cpp/onnxruntime cmake build is a"
log "          cargo-xwin limitation, not a real-CI signal — prove that on Windows."

cd sovereign/crates/sovereign-desktop
# ${arr[@]+...} guards the empty-array expansion so macOS's bash 3.2 doesn't
# trip `set -u` ("unbound variable") when no --features were passed.
exec cargo xwin "$CARGO_VERB" --target "$TARGET" ${PASSTHRU[@]+"${PASSTHRU[@]}"}
