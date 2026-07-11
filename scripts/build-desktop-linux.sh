#!/usr/bin/env bash
# build-desktop-linux.sh . one-command driver for the local Linux
# desktop bundle build.
#
# Runs the Containerfile.linux-build image with the workspace mounted
# at /work, isolated cargo + npm caches so container builds don't
# stomp on your host's, and TAURI_SIGNING_PRIVATE_KEY{,_PASSWORD}
# forwarded from your shell env if set.
#
# Usage:
#   scripts/build-desktop-linux.sh                 # unsigned build
#
#   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/sovereign-updater.key)" \
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... \
#       scripts/build-desktop-linux.sh             # signed build
#
#   scripts/build-desktop-linux.sh --rebuild       # force image rebuild
#   scripts/build-desktop-linux.sh --shell         # drop into the
#                                                    container at /work
#                                                    instead of building
#
# Outputs (visible on host):
#   target-container-linux/x86_64-unknown-linux-gnu/release/bundle/
#     appimage/Sovereign_<ver>_amd64.AppImage[.sig]
#     deb/sovereign_<ver>_amd64.deb

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

IMAGE="sovereign-desktop-linux-build:latest"
CONTAINERFILE="sovereign/crates/sovereign-desktop/containerfiles/Containerfile.linux-build"

# ─── Runtime: podman preferred (Fedora native, rootless), docker fallback ───
if command -v podman >/dev/null 2>&1; then
    RUNTIME="podman"
elif command -v docker >/dev/null 2>&1; then
    RUNTIME="docker"
else
    echo "build-desktop-linux: need podman or docker on PATH" >&2
    exit 2
fi

# ─── Flags ────────────────────────────────────────────────────────────
REBUILD=0
SHELL_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --rebuild) REBUILD=1 ;;
        --shell)   SHELL_ONLY=1 ;;
        -h|--help)
            sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "build-desktop-linux: unknown arg: $arg" >&2
            exit 2
            ;;
    esac
done

# ─── Build image if needed ────────────────────────────────────────────
needs_build=0
if [[ "$REBUILD" == "1" ]]; then
    needs_build=1
elif ! $RUNTIME image exists "$IMAGE" 2>/dev/null; then
    # `docker` doesn't have `image exists`; fall back to `inspect`.
    if ! $RUNTIME image inspect "$IMAGE" >/dev/null 2>&1; then
        needs_build=1
    fi
fi

# Staleness check: rebuild when the Containerfile or entrypoint is newer
# than the image. The entrypoint is BAKED into the image at build time, so
# an existing image silently drops later fixes — the 0.1.20 cut ran a
# pre-AppImage-fixes entrypoint this way and the leg failed.
if (( ! needs_build )); then
    img_epoch=$(python3 -c "
from datetime import datetime; import sys
try: print(int(datetime.fromisoformat(sys.argv[1].split('.')[0] + '+00:00').timestamp()))
except Exception: print(0)" "$($RUNTIME image inspect "$IMAGE" --format '{{.Created}}' 2>/dev/null | sed 's/ /T/;s/ .*//')" 2>/dev/null || echo 0)
    for f in "$CONTAINERFILE" "$(dirname "$CONTAINERFILE")/build-entrypoint.sh"; do
        if [[ -f "$f" ]] && (( $(stat -f %m "$f") > img_epoch )); then
            echo "[build-desktop-linux] $f is newer than the image — rebuilding."
            needs_build=1
        fi
    done
fi

if (( needs_build )); then
    echo "[build-desktop-linux] Building image $IMAGE (one-time, ~10 minutes)..."
    # Pin amd64: the build targets x86_64-unknown-linux-gnu and the LunarG
    # Vulkan apt repo only ships amd64 packages — on an arm64 host (Apple
    # Silicon + Rosetta/qemu) the default host-arch image build fails at
    # apt with "vulkan-headers has no installation candidate".
    $RUNTIME build --platform linux/amd64 -t "$IMAGE" -f "$CONTAINERFILE" .
fi

# ─── Prepare host-side cache dirs (visible to container at /work/...) ──
mkdir -p target-container-linux .cargo-container .npm-container \
         .tauri-cache-container .npm-container-modules

# ─── Run ──────────────────────────────────────────────────────────────
# Forward signing env if present; pass empty strings otherwise so the
# entrypoint's logging can detect "not set" without ambiguity.
RUN_ARGS=(
    --rm
    --platform linux/amd64
    -v "$REPO_ROOT:/work:Z"
    -e "TAURI_SIGNING_PRIVATE_KEY=${TAURI_SIGNING_PRIVATE_KEY:-}"
    -e "TAURI_SIGNING_PRIVATE_KEY_PASSWORD=${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
    -e "SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH=${SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH:-0}"
    # linuxdeploy is itself an AppImage; containers have no FUSE, so tell it
    # (and its plugins) to self-extract instead of FUSE-mounting.
    -e "APPIMAGE_EXTRACT_AND_RUN=1"
    # Persist tauri-bundler's tool downloads (linuxdeploy + plugins) across
    # runs, and let the entrypoint patch the appimage plugin's magic bytes
    # (see build-entrypoint.sh) before the bundler executes it.
    -v "$REPO_ROOT/.tauri-cache-container:/root/.cache/tauri:Z"
    # Shadow the package's node_modules with a container-private dir.
    # Without this, the entrypoint's `npm ci` replaces the host's
    # darwin-arm64 native binaries (esbuild, rollup) with linux-x64 ones
    # and host `npm run build` breaks until you rm -rf + npm ci again.
    -v "$REPO_ROOT/.npm-container-modules:/work/sovereign/crates/sovereign-desktop/node_modules:Z"
)

if (( SHELL_ONLY )); then
    $RUNTIME run "${RUN_ARGS[@]}" -it --entrypoint /bin/bash "$IMAGE"
    exit $?
fi

$RUNTIME run "${RUN_ARGS[@]}" "$IMAGE"

# ─── Surface results ──────────────────────────────────────────────────
BUNDLE_DIR="target-container-linux/x86_64-unknown-linux-gnu/release/bundle"
if [[ -d "$BUNDLE_DIR" ]]; then
    echo
    echo "Artifacts on host:"
    ls -lh "$BUNDLE_DIR"/appimage/*.AppImage "$BUNDLE_DIR"/appimage/*.AppImage.sig "$BUNDLE_DIR"/deb/*.deb 2>/dev/null || true
fi
