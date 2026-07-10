#!/usr/bin/env bash
# build-desktop-macos.sh . local macOS desktop bundle build, run
# natively on a Mac (cannot be containerized . Apple's macOS license
# forbids virtualization on non-Apple hardware).
#
# Mirrors the GitHub Actions `macos-14` / `macos-13` runner steps:
#   1. Verify Xcode CLT + SDKROOT (avoids the bindgen "memory file
#      not found" error per [[feedback_macos_sdkroot_for_bindgen]]).
#   2. brew install tesseract + lld + protobuf + cmake. lld is needed by
#      .cargo/config.toml's *-apple-darwin rustflags; protobuf provides
#      protoc (lance-encoding's prost-build); cmake builds llama.cpp's
#      Metal backend.
#   3. Stage the tesseract binary into binaries/<triple>/.
#   4. Run fetch-desktop-binaries.sh for PDFium + tessdata.
#   5. cargo tauri build against the release config — which DEEP AD-HOC
#      code-signs the .app (bundle.macOS.signingIdentity = "-"), so the
#      app and its nested binaries (daemon, tesseract, pdfium) run on
#      Apple Silicon. Ad-hoc is NOT notarization — recipients still clear
#      Gatekeeper quarantine once (printed at the end).
#   6. Print bundle locations + the friend-install note.
#
# Targets:
#   --target aarch64-apple-darwin   (default on Apple Silicon)
#   --target x86_64-apple-darwin    (default on Intel; cross-builds OK on Apple Silicon via Rosetta)
#   --universal                     (universal2 binary . both arches in one bundle)
#
# Usage:
#   scripts/build-desktop-macos.sh                  # auto-detect host arch
#   scripts/build-desktop-macos.sh --target x86_64-apple-darwin
#   scripts/build-desktop-macos.sh --universal
#
#   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/sovereign-updater.key)" \
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... \
#       scripts/build-desktop-macos.sh              # signed build
#
# Output (visible on host):
#   target/<triple>/release/bundle/
#     dmg/svrnmesh_<ver>_<arch>.dmg
#     macos/svrnmesh.app.tar.gz[.sig]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

log() { printf '\n[build-desktop-macos] %s\n' "$*"; }

# ─── Host check ───────────────────────────────────────────────────────
if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-desktop-macos: this script must run on macOS." >&2
    echo "  Linux/other: containers + osxcross don't reliably produce" >&2
    echo "  a working Tauri .app/.dmg. Use a Mac." >&2
    exit 2
fi

# ─── Target selection ────────────────────────────────────────────────
TARGET=""
UNIVERSAL=0
for arg in "$@"; do
    case "$arg" in
        --target)        : ;;
        --target=*)      TARGET="${arg#--target=}" ;;
        --universal)     UNIVERSAL=1 ;;
        aarch64-apple-darwin|x86_64-apple-darwin) TARGET="$arg" ;;
        *)
            if [[ -z "${TARGET:-}" && -z "$arg" ]]; then
                :
            elif [[ "$arg" == "--help" || "$arg" == "-h" ]]; then
                sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//'
                exit 0
            fi
            ;;
    esac
done

# Handle bare positional and resolve default.
if [[ "$UNIVERSAL" == "0" && -z "$TARGET" ]]; then
    case "$(uname -m)" in
        arm64)  TARGET="aarch64-apple-darwin" ;;
        x86_64) TARGET="x86_64-apple-darwin" ;;
        *)      echo "build-desktop-macos: unsupported arch $(uname -m)" >&2; exit 2 ;;
    esac
fi

if (( UNIVERSAL )); then
    log "Target: universal-apple-darwin (both arches in one bundle)"
    TARGET_ARG="--target universal-apple-darwin"
    OUT_DIR="target/universal-apple-darwin/release/bundle"
else
    log "Target: $TARGET"
    TARGET_ARG="--target $TARGET"
    OUT_DIR="target/$TARGET/release/bundle"
fi

# ─── Xcode CLT + SDKROOT (bindgen needs it for llama-cpp-sys-4) ──────
if ! xcode-select -p >/dev/null 2>&1; then
    log "Xcode Command Line Tools not installed."
    log "Run: xcode-select --install"
    log "Then re-run this script."
    exit 2
fi
export SDKROOT="${SDKROOT:-$(xcrun --show-sdk-path)}"
log "SDKROOT: $SDKROOT"

# ggml-backend-dl.cpp uses std::filesystem (introduced macOS 10.15); the cc
# crate that llama-cpp-sys-4's cmake build leans on defaults lower. Export so
# it reaches the build script's compiler invocation (matches CI + the root
# .cargo/config.toml [env]).
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-10.15}"
log "MACOSX_DEPLOYMENT_TARGET: $MACOSX_DEPLOYMENT_TARGET"

# ─── Homebrew deps ────────────────────────────────────────────────────
if ! command -v brew >/dev/null 2>&1; then
    log "Homebrew not installed. Install from https://brew.sh and retry."
    exit 2
fi

ensure_brew() {
    local pkg="$1"
    if brew list --formula --versions "$pkg" >/dev/null 2>&1; then
        log "  $pkg already installed"
    else
        log "  installing $pkg via brew..."
        brew install "$pkg"
    fi
}

log "Checking brew deps..."
ensure_brew tesseract
ensure_brew lld
ensure_brew protobuf   # protoc — lance-encoding's prost-build step shells out to it
ensure_brew cmake      # llama-cpp-sys-4 compiles llama.cpp's Metal backend with it

# ─── Rust toolchain target ───────────────────────────────────────────
log "Ensuring rust target is installed..."
if (( UNIVERSAL )); then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
else
    rustup target add "$TARGET"
fi

# ─── Tauri CLI ───────────────────────────────────────────────────────
if ! cargo tauri --version >/dev/null 2>&1; then
    log "Installing tauri-cli..."
    cargo install tauri-cli --version "^2.0.0" --locked
fi

# ─── Stage external binaries ─────────────────────────────────────────
BIN_DIR="sovereign/crates/sovereign-desktop/src-tauri/binaries"
mkdir -p "$BIN_DIR"

stage_tesseract_for() {
    local triple="$1"
    # A prior staging copies brew's read-only mode (r-xr-xr-x) onto the dest,
    # so a plain cp over it fails with "Permission denied" — remove first.
    rm -f "$BIN_DIR/tesseract-$triple"
    cp "$(brew --prefix tesseract)/bin/tesseract" "$BIN_DIR/tesseract-$triple"
    log "  Staged tesseract for $triple"
}

if (( UNIVERSAL )); then
    stage_tesseract_for "aarch64-apple-darwin"
    stage_tesseract_for "x86_64-apple-darwin"
    bash scripts/fetch-desktop-binaries.sh "aarch64-apple-darwin"
    bash scripts/fetch-desktop-binaries.sh "x86_64-apple-darwin"
else
    stage_tesseract_for "$TARGET"
    bash scripts/fetch-desktop-binaries.sh "$TARGET"
fi

# ─── Frontend deps ────────────────────────────────────────────────────
log "Installing npm deps..."
(cd sovereign/crates/sovereign-desktop && npm ci --no-audit --no-fund)

# ─── Signing visibility ──────────────────────────────────────────────
# Two independent signatures are in play; don't conflate them:
#  - Code signing (Gatekeeper / runnability): tauri.release.conf.json sets
#    bundle.macOS.signingIdentity = "-", so `cargo tauri build` DEEP
#    AD-HOC signs the .app during bundling. That's what makes the app and
#    its nested binaries run on Apple Silicon at all. It does NOT satisfy
#    Gatekeeper — recipients clear quarantine once (note printed below).
#    A real Developer ID (Phase 2 in RELEASING.md) removes that prompt.
#  - Updater signing (.sig sidecars): driven by TAURI_SIGNING_PRIVATE_KEY,
#    consumed by the in-app updater — unrelated to launching the app.
log "Code signing: ad-hoc (bundle.macOS.signingIdentity = \"-\")"
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    log "Updater: TAURI_SIGNING_PRIVATE_KEY not set — no .sig sidecars (fine for hand-shared builds; the in-app updater just won't engage)."
fi

# ─── Cross-build: keep host-arch OpenSSL out of llama.cpp ────────────
# When TARGET != host arch, llama.cpp's LLAMA_OPENSSL=ON finds the HOST
# Homebrew OpenSSL (arm64) and the mtmd tool binaries fail to link for
# x86_64 ("_X509_* not found"). llama-cpp-sys-4 passes no
# -DCMAKE_TOOLCHAIN_FILE on apple targets, so cmake >=3.21 honors the env
# var — inject a fragment that forces LLAMA_OPENSSL OFF.
HOST_TRIPLE="$( [[ "$(uname -m)" == "arm64" ]] && echo aarch64-apple-darwin || echo x86_64-apple-darwin )"
if [[ "$TARGET" != "$HOST_TRIPLE" ]]; then
    export CMAKE_TOOLCHAIN_FILE="$REPO_ROOT/scripts/cmake/darwin-cross-no-openssl.cmake"
    log "Cross build ($HOST_TRIPLE -> $TARGET): CMAKE_TOOLCHAIN_FILE=$CMAKE_TOOLCHAIN_FILE (LLAMA_OPENSSL=OFF)"
fi

# ─── Build ───────────────────────────────────────────────────────────
# Capture cargo's real exit code. Do NOT pipe this through `tee`/`| …`
# or chain it with `; echo` — a pipeline's status is the LAST stage's, so
# either masks a failed build as success (observed 2026-06-18: a failed
# DMG bundling reported exit 0 because of `| tee`).
log "Running cargo tauri build $TARGET_ARG ..."
set +e
(cd sovereign/crates/sovereign-desktop && cargo tauri build \
    $TARGET_ARG \
    --config src-tauri/tauri.release.conf.json)
BUILD_RC=$?
set -e

# Tauri names the bundle after productName in tauri.conf.json ("svrnmesh"
# since the 2026-06-29 rename). Glob instead of hardcoding so a rename can't
# silently disable the TCC fallback below (which is exactly what happened
# for desktop-v0.1.19: the old "Sovereign.app" path made a Finder-denied DMG
# step look like a compile failure).
APP="$(find "$OUT_DIR/macos" -maxdepth 1 -name '*.app' -print -quit 2>/dev/null || true)"
APP_NAME="$(basename "${APP:-svrnmesh.app}" .app)"

if (( BUILD_RC != 0 )); then
    # The most common LOCAL failure is the DMG cosmetic step, NOT the build.
    # Tauri's bundle_dmg.sh runs an osascript that asks Finder to set the DMG
    # window's background/icon layout; that needs permission to send Apple
    # Events to Finder, which any process outside an interactive Aqua/GUI
    # session (SSH, CI agent, an automation tool's shell) is denied —
    # `Not authorized to send Apple events to Finder. (-1743)`. create-dmg
    # then exits 64 and Tauri reports a generic "error running bundle_dmg.sh",
    # swallowing the real cause. The .app itself is already built + deep
    # ad-hoc signed; only the DMG packaging failed.
    #
    # Detect that exact shape — .app present, no .dmg — and finish the job with
    # a plain hdiutil-built DMG (functionally identical: a compressed,
    # drag-to-install image; it just lacks the background picture). A genuine
    # compile/link failure leaves no .app, and we re-raise the original exit
    # code instead of masking it.
    if [[ -n "$APP" && -d "$APP" ]] && ! ls "$OUT_DIR"/dmg/*.dmg >/dev/null 2>&1; then
        if osascript -e 'tell application "Finder" to count windows' >/dev/null 2>&1; then
            log "Tauri DMG step failed though Finder IS scriptable here — packaging via hdiutil anyway. Check the cargo output above for the underlying cause."
        else
            log "DMG cosmetic step needs Finder Apple-Events, denied in this non-GUI context (-1743). The .app built fine; packaging the DMG via hdiutil (needs no Finder)."
        fi
        VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
        case "$TARGET" in
            aarch64-apple-darwin) ARCH_SUFFIX=aarch64 ;;
            x86_64-apple-darwin)  ARCH_SUFFIX=x64 ;;
            *)                    ARCH_SUFFIX="$(uname -m)" ;;
        esac
        (( UNIVERSAL )) && ARCH_SUFFIX=universal
        DMG_OUT="$OUT_DIR/dmg/${APP_NAME}_${VERSION}_${ARCH_SUFFIX}.dmg"
        rm -f "$OUT_DIR"/macos/rw.*.dmg   # create-dmg's leftover read-write scratch image
        STAGE="$(mktemp -d /tmp/sovereign-dmg.XXXXXX)"
        ditto "$APP" "$STAGE/$APP_NAME.app"        # ditto preserves the ad-hoc signature
        ln -s /Applications "$STAGE/Applications"   # drag-to-install target
        mkdir -p "$OUT_DIR/dmg"
        rm -f "$DMG_OUT"
        hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG_OUT"
        rm -rf "$STAGE"
        log "DMG built via hdiutil fallback: $DMG_OUT"
    else
        log "Build failed before the DMG step (no .app bundle produced) — this is a real build error, not the Finder/TCC cosmetic issue. See the cargo output above."
        exit "$BUILD_RC"
    fi
fi

# ─── Surface results ─────────────────────────────────────────────────
log "Build complete. Bundles:"
shopt -s nullglob
for f in "$OUT_DIR"/dmg/*.dmg \
         "$OUT_DIR"/macos/*.app.tar.gz \
         "$OUT_DIR"/macos/*.app.tar.gz.sig; do
    printf '  %s\n' "$f"
done

cat <<'EOF'

Sharing with friends:
  This build is ad-hoc signed (runs on Apple Silicon) but NOT notarized,
  so macOS Gatekeeper warns on first launch. Tell recipients to either:
    • right-click the app → Open → Open, or
    • run once:  xattr -dr com.apple.quarantine /Applications/svrnmesh.app
  (A Developer ID + notarization removes this — see RELEASING.md Phase 2.)
EOF
