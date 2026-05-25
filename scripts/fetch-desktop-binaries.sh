#!/usr/bin/env bash
# fetch-desktop-binaries.sh — stage external binaries the Sovereign desktop
# bundles for the OCR pipeline (Tesseract, PDFium, tessdata).
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
# When called without an argument, the script auto-detects the host's
# triple via `rustc -vV`. CI passes the matrix triple explicitly.
#
# Idempotent: existing files are not re-downloaded. Re-run safely.
#
# What this script DOES fetch:
#   - PDFium dylib (bblanchon/pdfium-binaries, latest release)
#   - Tesseract eng.traineddata (tesseract-ocr/tessdata, main)
#   - On Windows: a static-ish tesseract.exe from UB Mannheim
#
# What this script does NOT fetch (yet):
#   - macOS / Linux Tesseract binaries. They have non-trivial dynamic
#     dependencies (libleptonica, libtiff, libjpeg, libpng) that need
#     to be bundled together to be portable. For v1, the desktop's
#     OCR-availability probe accepts a system-installed tesseract:
#     macOS users `brew install tesseract`, Linux users
#     `apt install tesseract-ocr`. Phase 2 of RELEASING.md tracks
#     building / hosting static binaries we can ship.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
DESKTOP_BIN_DIR="${REPO_ROOT}/sovereign/crates/sovereign-desktop/src-tauri/binaries"

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
mkdir -p "$DESKTOP_BIN_DIR" "$DESKTOP_BIN_DIR/tessdata" "$DESKTOP_BIN_DIR/pdfium"

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

# ─── tessdata (English) ─────────────────────────────────────────────

TESSDATA_URL="https://github.com/tesseract-ocr/tessdata/raw/main/eng.traineddata"
TESSDATA_PATH="$DESKTOP_BIN_DIR/tessdata/eng.traineddata"
echo
echo "[1/3] tessdata"
fetch_to "$TESSDATA_URL" "$TESSDATA_PATH"

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
echo "[2/3] PDFium ($PDFIUM_PLATFORM)"
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
    fi
    rm -rf "$PDFIUM_TMP"
    trap - EXIT
fi

# ─── Tesseract ──────────────────────────────────────────────────────

echo
echo "[3/3] Tesseract"
TESSERACT_DEST="$DESKTOP_BIN_DIR/tesseract-${TARGET}"
TESSERACT_DEST_EXE="${TESSERACT_DEST}.exe"

case "$TARGET" in
    x86_64-pc-windows-msvc)
        # UB Mannheim ships a portable tesseract.exe. The release page
        # changes URLs over time, so we pin a known-good archive name
        # and let the user override via TESSERACT_WIN_URL if needed.
        UB_URL="${TESSERACT_WIN_URL:-https://digi.bib.uni-mannheim.de/tesseract/tesseract-ocr-w64-setup-5.5.0.20241111.exe}"
        echo "  Windows: UB Mannheim distributable"
        echo "  url: $UB_URL"
        echo "  This is an installer, not a portable binary. Manual step:"
        echo "    1. Run the installer (or extract via 7-Zip)."
        echo "    2. Copy tesseract.exe to:"
        echo "       $TESSERACT_DEST_EXE"
        echo "    3. Re-run this script to verify."
        if [[ -f "$TESSERACT_DEST_EXE" ]]; then
            echo "  found: $TESSERACT_DEST_EXE"
        else
            echo "  NOT YET STAGED — see manual steps above."
        fi
        ;;
    aarch64-apple-darwin|x86_64-apple-darwin)
        if [[ -f "$TESSERACT_DEST" ]]; then
            echo "  found: $TESSERACT_DEST"
        else
            echo "  macOS: not auto-fetched in v1 (dynamic dep on brew dylibs)."
            echo "  Local dev path:"
            echo "    brew install tesseract"
            echo "    cp \"\$(brew --prefix tesseract)/bin/tesseract\" \\"
            echo "       $TESSERACT_DEST"
            echo "  See sovereign/crates/sovereign-desktop/RELEASING.md §'External binaries'"
            echo "  for the static-binary plan (Phase 2)."
        fi
        ;;
    x86_64-unknown-linux-gnu)
        if [[ -f "$TESSERACT_DEST" ]]; then
            echo "  found: $TESSERACT_DEST"
        else
            echo "  Linux: not auto-fetched in v1 (dynamic dep on libleptonica)."
            echo "  Local dev path:"
            echo "    sudo apt install tesseract-ocr"
            echo "    cp /usr/bin/tesseract $TESSERACT_DEST"
            echo "  See sovereign/crates/sovereign-desktop/RELEASING.md §'External binaries'"
            echo "  for the static-binary plan (Phase 2)."
        fi
        ;;
esac

# ─── Summary ────────────────────────────────────────────────────────

echo
echo "Summary:"
ls -la "$DESKTOP_BIN_DIR" 2>/dev/null || true
echo
echo "Next:"
echo "  1. If anything is missing above, follow the printed instructions."
echo "  2. cd sovereign/crates/sovereign-desktop && npm run tauri build"
echo "     (or use --config src-tauri/tauri.release.conf.json to bundle the binaries)"
