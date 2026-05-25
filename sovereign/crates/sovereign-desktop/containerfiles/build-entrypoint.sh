#!/usr/bin/env bash
# build-entrypoint.sh . runs inside Containerfile.linux-build.
#
# Mirrors the GitHub Actions workflow's per-platform steps for
# x86_64-unknown-linux-gnu, with one difference: we run from inside
# the container instead of being a shell script chained from a
# `working-directory:` block. Same outcome: AppImage + .deb in
# target-container-linux/x86_64-unknown-linux-gnu/release/bundle/.
#
# Reads from env (all optional):
#   TAURI_SIGNING_PRIVATE_KEY            - base64 updater private key
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD   - password from `tauri signer generate`
#   SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH - set to "1" to skip fetching
#                                          PDFium/tessdata (useful when
#                                          re-running on a workspace
#                                          that already has them).

set -euo pipefail

# Trace the high-level steps but keep the cargo output un-prefixed so
# Rust diagnostics stay parseable.
log() { printf '\n[build-desktop-linux] %s\n' "$*"; }

cd /work

log "Linux container build starting on $(uname -m)"

# ─── Stage external binaries (PDFium + tessdata + tesseract) ─────────
TARGET="x86_64-unknown-linux-gnu"
BIN_DIR="sovereign/crates/sovereign-desktop/src-tauri/binaries"
mkdir -p "$BIN_DIR"

# Tesseract: container apt installed /usr/bin/tesseract. Stage with
# the per-triple naming Tauri's externalBin expects.
cp /usr/bin/tesseract "$BIN_DIR/tesseract-${TARGET}"
log "Staged tesseract ($(tesseract --version 2>&1 | head -1))"

if [[ "${SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH:-0}" != "1" ]]; then
    log "Fetching PDFium + tessdata..."
    bash scripts/fetch-desktop-binaries.sh "$TARGET"
else
    log "Skipping PDFium + tessdata fetch (SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH=1)"
fi

# ─── Frontend deps ────────────────────────────────────────────────────
log "Installing npm deps..."
(cd sovereign/crates/sovereign-desktop && npm ci --no-audit --no-fund)

# ─── Signing key sanity check ────────────────────────────────────────
# tauri-bundler treats empty signing key as "skip signing" and proceeds
# without emitting .sig sidecars. That's fine for iteration but worth
# flagging clearly so it doesn't surprise anyone debugging the updater
# chain locally.
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    log "WARNING: TAURI_SIGNING_PRIVATE_KEY not set . AppImage will be UNSIGNED (no .sig sidecar)."
    log "         To sign locally: TAURI_SIGNING_PRIVATE_KEY=\"\$(cat ~/.tauri/sovereign-updater.key)\" \\"
    log "                           TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... scripts/build-desktop-linux.sh"
fi

# ─── Tauri build ──────────────────────────────────────────────────────
# ggml/llama are STATIC-linked into the binary (the vendored llama-cpp-4
# drops `dynamic-link` from its default features), so the bundle is
# self-contained: no libggml*.so / libllama*.so to locate or co-package,
# and linuxdeploy's AppImage dependency walk only sees packageable system
# libs (libvulkan.so.1, GTK, …). A single build pass produces all three
# installers. (If you re-enable `dynamic-link`, you'll need to put the
# ggml/llama .so dir on LD_LIBRARY_PATH before bundling so linuxdeploy can
# resolve them — see RELEASING.md.)
log "Running cargo tauri build..."
(cd sovereign/crates/sovereign-desktop && cargo tauri build \
    --target "$TARGET" \
    --config src-tauri/tauri.release.conf.json)

# ─── Report what landed ──────────────────────────────────────────────
BUNDLE_DIR="${CARGO_TARGET_DIR:-/work/target-container-linux}/${TARGET}/release/bundle"
log "Build complete. Bundles:"
shopt -s nullglob
for f in "$BUNDLE_DIR/appimage"/*.AppImage \
         "$BUNDLE_DIR/appimage"/*.AppImage.sig \
         "$BUNDLE_DIR/deb"/*.deb; do
    printf '  %s  %s\n' "$(stat -c '%s' "$f" | numfmt --to=iec --suffix=B --padding=8)" "$f"
done

log "Done."
