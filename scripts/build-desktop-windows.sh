#!/usr/bin/env bash
# build-desktop-windows.sh — containerized Windows desktop build, run on
# the arm64 Mac. Cross-compiles x86_64-pc-windows-msvc inside a NATIVE
# arm64 Linux container (cargo-xwin + clang-cl + lld-link + NSIS) — no
# Rosetta/qemu emulation, unlike the Linux leg.
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

RUNTIME="${CONTAINER_RUNTIME:-podman}"
IMAGE="sovereign-desktop-windows-build:latest"
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

if (( REBUILD_IMAGE )) || ! "$RUNTIME" image exists "$IMAGE"; then
    log "Building image $IMAGE (native arm64)..."
    "$RUNTIME" build --platform linux/arm64 -t "$IMAGE" -f "$CONTAINERFILE" .
fi

# Isolated caches, same pattern as the Linux leg — plus the xwin MSVC
# CRT/SDK cache so the ~1GB download happens once.
mkdir -p target-container-windows .cargo-container-windows .npm-container \
         .xwin-container .tauri-cache-container .npm-container-modules-windows \
         .ort-cache-container

RUN_ARGS=(
    --rm
    --platform linux/arm64
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
