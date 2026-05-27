#!/usr/bin/env bash
# fetch-desktop-binaries.sh — stage the external assets the Sovereign
# desktop bundles for the OCR pipeline: the PaddleOCR ONNX models and the
# PDFium rasterization library.
#
# Usage:
#   scripts/fetch-desktop-binaries.sh [<target_triple>]
#
# Target triples (matches Tauri's bundle naming):
#   aarch64-apple-darwin            — macOS, Apple Silicon
#   x86_64-apple-darwin             — macOS, Intel
#   x86_64-unknown-linux-gnu        — Linux, x86_64
#   x86_64-pc-windows-msvc          — Windows, x86_64
#
# The triple only selects the PDFium platform build — the PaddleOCR models
# are ONNX and platform-independent. Without an argument the script
# auto-detects the host triple via `rustc -vV`; CI passes it explicitly.
#
# Idempotent: existing files are not re-downloaded. Re-run safely.
#
# What this fetches:
#   - PaddleOCR det + rec ONNX models and the recognition dictionary
#     (HF: SWHL/RapidOCR + monkt/paddleocr-onnx, Apache-2.0) → ~13 MB
#   - PDFium shared library (bblanchon/pdfium-binaries, latest)      → ~7 MB
#
# What it does NOT fetch: tesseract. The 2026-05-27 bake-off (see
# sovereign/docs/OCR_PADDLE_ENGINE.md) replaced tesseract with PaddleOCR —
# which needs no platform install — so the desktop no longer bundles it.
# Tesseract remains a CODE fallback (OcrEngineKind::Tesseract) for users
# with a system install; a tesseract-bundling build is a documented opt-in
# in RELEASING.md (restore the externalBin/tessdata entries + stage a
# statically linked binary).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
DESKTOP_BIN_DIR="${REPO_ROOT}/sovereign/crates/sovereign-desktop/src-tauri/binaries"

# PaddleOCR model set id — must match `paddle::DEFAULT_MODEL_ID` and the
# `tauri.release.conf.json` resources glob.
PADDLE_MODEL_ID="ppocr-en-v4v5"

# ─── Target detection ───────────────────────────────────────────────

target_triple_arg="${1:-}"
if [[ -n "$target_triple_arg" ]]; then
    TARGET="$target_triple_arg"
else
    if ! command -v rustc >/dev/null 2>&1; then
        echo "fetch-desktop-binaries: rustc not on PATH; pass a target triple explicitly" >&2
        exit 2
    fi
    TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
fi

case "$TARGET" in
    aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu|x86_64-pc-windows-msvc) ;;
    *)
        echo "fetch-desktop-binaries: unsupported target triple '$TARGET'" >&2
        echo "supported: aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc" >&2
        exit 2
        ;;
esac

echo "fetch-desktop-binaries: target=$TARGET dest=$DESKTOP_BIN_DIR"
PADDLE_DIR="$DESKTOP_BIN_DIR/paddle-ocr/$PADDLE_MODEL_ID"
mkdir -p "$PADDLE_DIR" "$DESKTOP_BIN_DIR/pdfium"

# ─── Helpers ────────────────────────────────────────────────────────

# fetch_to <url> <dest_path>  — idempotent download via curl
fetch_to() {
    local url="$1" dest="$2"
    if [[ -f "$dest" ]]; then
        echo "  skip: $(basename "$dest") (already present)"
        return 0
    fi
    echo "  fetch: $url"
    if ! curl -fsSL --retry 3 -o "$dest.partial" "$url"; then
        echo "fetch-desktop-binaries: download failed: $url" >&2
        rm -f "$dest.partial"
        return 1
    fi
    mv "$dest.partial" "$dest"
}

# extract_to <archive> <dest_dir>  — handle .tgz / .zip uniformly
extract_to() {
    local archive="$1" dest="$2"
    mkdir -p "$dest"
    case "$archive" in
        *.tgz|*.tar.gz) tar -xzf "$archive" -C "$dest" ;;
        *.zip)
            if command -v unzip >/dev/null 2>&1; then
                unzip -q -o "$archive" -d "$dest"
            else
                echo "fetch-desktop-binaries: unzip not found; install it or extract $archive manually" >&2
                return 1
            fi
            ;;
        *)
            echo "fetch-desktop-binaries: unknown archive format: $archive" >&2
            return 1
            ;;
    esac
}

# ─── PaddleOCR models (platform-independent) ─────────────────────────

# det: DBNet text detection. rec: CRNN/SVTR recognition (v5 English).
# dict: the rec model's CTC label table (det/rec/dict must be a matched
# set — the recognizer warns on a dict/class-count mismatch >1).
DET_URL="https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx"
REC_URL="https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/rec.onnx"
DICT_URL="https://huggingface.co/monkt/paddleocr-onnx/resolve/main/languages/english/dict.txt"

echo
echo "[1/2] PaddleOCR models ($PADDLE_MODEL_ID)"
paddle_ok=1
fetch_to "$DET_URL"  "$PADDLE_DIR/det.onnx"  || paddle_ok=0
fetch_to "$REC_URL"  "$PADDLE_DIR/rec.onnx"  || paddle_ok=0
fetch_to "$DICT_URL" "$PADDLE_DIR/dict.txt"  || paddle_ok=0
if [[ "$paddle_ok" -eq 1 ]]; then
    # Cheap sanity check: a non-trivial dict and a multi-MB rec model.
    dict_lines="$(wc -l < "$PADDLE_DIR/dict.txt" 2>/dev/null || echo 0)"
    echo "  ok: det.onnx + rec.onnx + dict.txt ($dict_lines dict lines)"
else
    echo "fetch-desktop-binaries: one or more PaddleOCR model files failed" >&2
    exit 1
fi

# ─── PDFium ─────────────────────────────────────────────────────────

# bblanchon/pdfium-binaries publishes archives keyed by the platform
# label they use, not by Rust target triple. Map between the two here.
case "$TARGET" in
    aarch64-apple-darwin)      PDFIUM_PLATFORM="mac-arm64"  PDFIUM_LIB="libpdfium.dylib" ;;
    x86_64-apple-darwin)       PDFIUM_PLATFORM="mac-x64"    PDFIUM_LIB="libpdfium.dylib" ;;
    x86_64-unknown-linux-gnu)  PDFIUM_PLATFORM="linux-x64"  PDFIUM_LIB="libpdfium.so" ;;
    x86_64-pc-windows-msvc)    PDFIUM_PLATFORM="win-x64"    PDFIUM_LIB="pdfium.dll" ;;
esac

PDFIUM_URL="https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-${PDFIUM_PLATFORM}.tgz"
PDFIUM_DEST="$DESKTOP_BIN_DIR/pdfium/$PDFIUM_LIB"

echo
echo "[2/2] PDFium ($PDFIUM_PLATFORM)"
if [[ -f "$PDFIUM_DEST" ]]; then
    echo "  skip: $PDFIUM_LIB (already present)"
else
    PDFIUM_TMP="$(mktemp -d)"
    trap 'rm -rf "$PDFIUM_TMP"' EXIT
    if fetch_to "$PDFIUM_URL" "$PDFIUM_TMP/pdfium.tgz" \
       && extract_to "$PDFIUM_TMP/pdfium.tgz" "$PDFIUM_TMP/extract"; then
        # PDFium archive layout differs by platform. Unix archives put the
        # shared lib in lib/ (libpdfium.dylib / libpdfium.so). Windows puts
        # the *runtime* DLL in bin/pdfium.dll — lib/ only holds the
        # pdfium.dll.lib import library, which we don't ship. Search both.
        pdfium_src=""
        for sub in lib bin; do
            if [[ -f "$PDFIUM_TMP/extract/$sub/$PDFIUM_LIB" ]]; then
                pdfium_src="$PDFIUM_TMP/extract/$sub/$PDFIUM_LIB"
                break
            fi
        done
        if [[ -n "$pdfium_src" ]]; then
            cp "$pdfium_src" "$PDFIUM_DEST"
            echo "  installed: $PDFIUM_DEST (from ${pdfium_src#"$PDFIUM_TMP/extract/"})"
        else
            echo "fetch-desktop-binaries: $PDFIUM_LIB not found in archive (looked in lib/, bin/)" >&2
            exit 1
        fi
    else
        echo "fetch-desktop-binaries: PDFium fetch/extract failed" >&2
        exit 1
    fi
    rm -rf "$PDFIUM_TMP"
    trap - EXIT
fi

# ─── Summary ────────────────────────────────────────────────────────

echo
echo "Summary:"
echo "  paddle-ocr/$PADDLE_MODEL_ID/:"
ls -la "$PADDLE_DIR" 2>/dev/null | sed 's/^/    /' || true
echo "  pdfium/:"
ls -la "$DESKTOP_BIN_DIR/pdfium" 2>/dev/null | sed 's/^/    /' || true
echo
echo "Next:"
echo "  cd sovereign/crates/sovereign-desktop"
echo "  cargo tauri build --config src-tauri/tauri.release.conf.json"
echo "  (omit --config for a plain dev build; the base config has no resources)"
