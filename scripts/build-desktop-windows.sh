#!/usr/bin/env bash
# build-desktop-windows.sh — containerized Windows desktop build. Cross-
# compiles x86_64-pc-windows-msvc inside a HOST-ARCH Linux container
# (cargo-xwin + clang-cl + lld-link + NSIS) — no Rosetta/qemu emulation on
# either supported host, unlike the Linux leg on the Mac. Runs from the arm64
# Mac (arm64 container) or an x86_64 Linux host (amd64 container).
#
# Produces: target-container-windows/x86_64-pc-windows-msvc/release/bundle/
#             nsis/svrnmesh_<ver>_x64-setup.exe (+ .sig when signing env set)
#
# Usage:
#   scripts/build-desktop-windows.sh                 # build image if needed, run
#   scripts/build-desktop-windows.sh --rebuild-image # force image rebuild
#   scripts/build-desktop-windows.sh --shell         # drop into a shell in the image
#
#   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/sovereign-updater.key)" \
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... \
#       scripts/build-desktop-windows.sh             # signed build

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

log() { printf '\n[build-desktop-windows] %s\n' "$*"; }

# shellcheck source=lib/release-host.sh
. "$SCRIPT_DIR/lib/release-host.sh"

RUNTIME="${CONTAINER_RUNTIME:-podman}"
IMAGE="sovereign-desktop-windows-build:latest"
# This image is HOST-ARCH by design: cargo-xwin's clang-cl targets
# x86_64-pc-windows-msvc from any host, so running the container native is
# free speed. The arch was hardcoded to linux/arm64 for the Mac, which on an
# x86_64 Linux host would have pulled an arm64 base and emulated the whole
# leg — the exact cost the Mac version was written to avoid.
PLATFORM="$RELEASE_HOST_CONTAINER_PLATFORM"
CONTAINERFILE="sovereign/crates/sovereign-desktop/containerfiles/Containerfile.windows-build"

REBUILD_IMAGE=0 SHELL_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --rebuild-image) REBUILD_IMAGE=1 ;;
        --shell)         SHELL_ONLY=1 ;;
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

command -v "$RUNTIME" >/dev/null || { echo "$RUNTIME not found" >&2; exit 2; }
"$RUNTIME" machine start >/dev/null 2>&1 || true   # no-op if already running

NEEDS_BUILD=0
if (( REBUILD_IMAGE )) || ! "$RUNTIME" image exists "$IMAGE"; then
    NEEDS_BUILD=1
else
    # Staleness check: the entrypoint is BAKED into the image, so an
    # existing image silently drops later fixes (see build-desktop-linux.sh).
    IMG_EPOCH=$(python3 -c "
from datetime import datetime; import sys
try: print(int(datetime.fromisoformat(sys.argv[1].split('.')[0] + '+00:00').timestamp()))
except Exception: print(0)" "$($RUNTIME image inspect "$IMAGE" --format '{{.Created}}' 2>/dev/null | sed 's/ /T/;s/ .*//')" 2>/dev/null || echo 0)
    # release_file_mtime, not `stat -f %m` — see build-desktop-linux.sh.
    for f in "$CONTAINERFILE" "$(dirname "$CONTAINERFILE")/build-entrypoint-windows.sh"; do
        if [[ -f "$f" ]] && (( $(release_file_mtime "$f") > IMG_EPOCH )); then
            log "$f is newer than the image — rebuilding."
            NEEDS_BUILD=1
        fi
    done
fi
if (( NEEDS_BUILD )); then
    log "Building image $IMAGE (native $PLATFORM)..."
    "$RUNTIME" build --platform "$PLATFORM" -t "$IMAGE" -f "$CONTAINERFILE" .
fi

# Isolated caches, same pattern as the Linux leg — plus the xwin MSVC
# CRT/SDK cache so the ~1GB download happens once.
mkdir -p target-container-windows .cargo-container-windows .npm-container \
         .xwin-container .tauri-cache-container .npm-container-modules-windows \
         .ort-cache-container

RUN_ARGS=(
    --rm
    --platform "$PLATFORM"
    -v "$REPO_ROOT:/work:Z"
    -e "TAURI_SIGNING_PRIVATE_KEY=${TAURI_SIGNING_PRIVATE_KEY:-}"
    -e "TAURI_SIGNING_PRIVATE_KEY_PASSWORD=${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
    -e "SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH=${SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH:-0}"
    # Tauri's NSIS plugin downloads land here; persist across runs.
    -v "$REPO_ROOT/.tauri-cache-container:/root/.cache/tauri:Z"
    # ort-sys extracts pyke's prebuilt onnxruntime here and cargo CACHES
    # the resulting link-search path in the build fingerprint. In an
    # ephemeral container the next run inherits a path that no longer
    # exists ("could not find native static library onnxruntime") —
    # persist it.
    -v "$REPO_ROOT/.ort-cache-container:/root/.cache/ort.pyke.io:Z"
    # Shadow node_modules with a container-private dir (arm64-linux
    # natives) so the container's npm ci can't stomp the host's.
    -v "$REPO_ROOT/.npm-container-modules-windows:/work/sovereign/crates/sovereign-desktop/node_modules:Z"
)

if (( SHELL_ONLY )); then
    $RUNTIME run "${RUN_ARGS[@]}" -it --entrypoint /bin/bash "$IMAGE"
    exit $?
fi

$RUNTIME run "${RUN_ARGS[@]}" "$IMAGE"
