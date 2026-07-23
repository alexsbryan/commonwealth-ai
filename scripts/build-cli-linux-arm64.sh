#!/usr/bin/env bash
# build-cli-linux-arm64.sh — build the three CLI binaries for
# aarch64-unknown-linux-gnu, NATIVELY, in an arm64 Linux container.
#
# On an Apple Silicon host this runs at native speed (arm64 containers, no
# qemu), unlike the x86_64 Linux leg. The output is meant for LOCAL soak runs
# via `scripts/cli-smoke.sh --install-mode binary --platform linux/arm64
# --binary target-container-linux-arm64/aarch64-unknown-linux-gnu/release`.
#
# It is NOT a release path (the shipped Linux target is x86_64; see
# release-cli-local.sh). This exists purely so a full golden-path soak can run
# locally at usable speed.
#
# Usage:
#   scripts/build-cli-linux-arm64.sh            # build image if needed, then the CLI
#   RUNTIME=docker scripts/build-cli-linux-arm64.sh
#   scripts/build-cli-linux-arm64.sh --shell    # drop into the builder for debugging

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RUNTIME="${RUNTIME:-podman}"
command -v "$RUNTIME" >/dev/null 2>&1 || { echo "need podman or docker (set RUNTIME=)"; exit 2; }

IMAGE="sovereign-cli-arm64-build:latest"
CF="sovereign/container/Containerfile.cli-arm64"
OUT="target-container-linux-arm64"
CARGO_CACHE=".cargo-container-arm64"

log() { printf '\n[build-cli-arm64] %s\n' "$*"; }

mkdir -p "$OUT" "$CARGO_CACHE"

# ── Build image if missing or the Containerfile changed ──────────────────────
# Stamp-file staleness (robust; no fragile date parsing): the stamp is touched
# after a successful image build, so `stamp newer than Containerfile` ⇒ current.
STAMP="$OUT/.image-built"
needs_build=1
if "$RUNTIME" image exists "$IMAGE" 2>/dev/null && [ -f "$STAMP" ] && [ "$STAMP" -nt "$CF" ]; then
  needs_build=0
fi
if [ "$needs_build" = 1 ]; then
  log "building image $IMAGE (one-time, ~5-10 min)…"
  "$RUNTIME" build --platform linux/arm64 -t "$IMAGE" -f "$CF" .
  touch "$STAMP"
fi

if [ "${1:-}" = "--shell" ]; then
  exec "$RUNTIME" run --rm -it --platform linux/arm64 \
    -v "$REPO_ROOT:/work:Z" \
    -e CARGO_TARGET_DIR="/work/$OUT" \
    -e CARGO_HOME="/work/$CARGO_CACHE" \
    "$IMAGE" /bin/bash
fi

# ── Build the three CLI binaries natively for aarch64-linux ──────────────────
log "compiling sovereign-cli + siblings for aarch64-unknown-linux-gnu…"
"$RUNTIME" run --rm --platform linux/arm64 \
  -v "$REPO_ROOT:/work:Z" \
  -e CARGO_TARGET_DIR="/work/$OUT" \
  -e CARGO_HOME="/work/$CARGO_CACHE" \
  "$IMAGE" \
  bash -lc '
    # aarch64 portability patch: pdfium-render 0.9.2 hardcodes `as *const i8`
    # where C `char` (c_char) is unsigned (u8) on ARM — a compile error the
    # x86-only release path never hits. Rewrite to c_char (identical to i8 on
    # x86, correct on ARM). Idempotent; applied to the extracted registry src.
    find "$CARGO_HOME"/registry/src -path "*pdfium-render-*/src/pdf/font/provider.rs" \
      -exec sed -i "s/as \*const i8, chars\.len()/as *const std::os::raw::c_char, chars.len()/g" {} + 2>/dev/null || true
    cargo build --release --locked --target aarch64-unknown-linux-gnu \
      -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm'

BIN_DIR="$OUT/aarch64-unknown-linux-gnu/release"
log "done. binaries:"
for b in sovereign-cli sovereign-cli-daemon sovereign-cli-llm; do
  if [ -f "$BIN_DIR/$b" ]; then
    printf '  %s (%s)\n' "$BIN_DIR/$b" "$(du -h "$BIN_DIR/$b" | cut -f1)"
  else
    printf '  MISSING: %s\n' "$BIN_DIR/$b"
  fi
done
log "soak it:  scripts/cli-smoke.sh --install-mode binary --platform linux/arm64 \\"
log "            --binary $BIN_DIR --model-dir <dir-of-tiny-gguf> --soak 30"
