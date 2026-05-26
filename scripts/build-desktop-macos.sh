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
#     dmg/Sovereign_<ver>_<arch>.dmg
#     macos/Sovereign.app.tar.gz[.sig]

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

# ─── Build ───────────────────────────────────────────────────────────
log "Running cargo tauri build $TARGET_ARG ..."
(cd sovereign/crates/sovereign-desktop && cargo tauri build \
    $TARGET_ARG \
    --config src-tauri/tauri.release.conf.json)

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
    • run once:  xattr -dr com.apple.quarantine /Applications/Sovereign.app
  (A Developer ID + notarization removes this — see RELEASING.md Phase 2.)
EOF
