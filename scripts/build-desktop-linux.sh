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

if (( needs_build )); then
    echo "[build-desktop-linux] Building image $IMAGE (one-time, ~10 minutes)..."
    $RUNTIME build -t "$IMAGE" -f "$CONTAINERFILE" .
fi

# ─── Prepare host-side cache dirs (visible to container at /work/...) ──
mkdir -p target-container-linux .cargo-container .npm-container

# ─── Run ──────────────────────────────────────────────────────────────
# Forward signing env if present; pass empty strings otherwise so the
# entrypoint's logging can detect "not set" without ambiguity.
RUN_ARGS=(
    --rm
    -v "$REPO_ROOT:/work:Z"
    -e "TAURI_SIGNING_PRIVATE_KEY=${TAURI_SIGNING_PRIVATE_KEY:-}"
    -e "TAURI_SIGNING_PRIVATE_KEY_PASSWORD=${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
    -e "SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH=${SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH:-0}"
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
