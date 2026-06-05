#!/usr/bin/env bash
# build-rpc-worker.sh — build a version-matched llama.cpp `rpc-server`
# (the mesh distributed-inference WORKER) from the exact llama.cpp the
# sovereign daemon links, so host↔worker speak the same RPC protocol.
#
# FALLBACK ONLY. The primary, distributable way to make a node a worker is
# to run the daemon with `SOVEREIGN_RPC_SERVE=0.0.0.0:50052` — it serves the
# local GPU in-process, no separate binary. See docs/RPC_DISTRIBUTED_INFERENCE.md.
# Use this script only to run a standalone worker WITHOUT the daemon.
#
# WHY this script exists
#   The embedded daemon engine (the RPC *host*) registers remote workers
#   via `SOVEREIGN_RPC_WORKERS` and offloads model layers to them. Each
#   worker is a standalone `rpc-server` process. The RPC wire protocol is
#   versioned, so the worker MUST be built from the same llama.cpp commit
#   the daemon links (pinned: llama.cpp b9180 / 64b38b561). Homebrew /
#   distro llama.cpp packages usually ship WITHOUT `-DGGML_RPC=ON` and at
#   a different version — they will not work.
#
#   This builds `rpc-server` (+ `llama-bench`, for a standalone link test)
#   from the source the `llama-cpp-sys-4` crate already checked out into
#   `target/.../build/llama-cpp-sys-4-*/out/llama.cpp`, enabling RPC plus
#   the right GPU backend for this host (Vulkan on Linux, Metal on macOS).
#
# PREREQUISITE
#   A prior workspace build must have compiled `llama-cpp-sys-4` so its
#   source tree exists. If you have not built yet:
#       cargo build -p sovereign-inference          # Linux: pulls Vulkan
#   then re-run this script.
#
# USAGE
#   scripts/build-rpc-worker.sh                 # build for this platform
#   scripts/build-rpc-worker.sh --backend cpu   # force CPU-only worker
#   scripts/build-rpc-worker.sh --run           # build, then exec rpc-server
#   RPC_PORT=50052 RPC_HOST=0.0.0.0 scripts/build-rpc-worker.sh --run
#
# After building, run the worker (bind 0.0.0.0 so a peer can reach it over
# Tailscale/LAN):
#   <repo>/target/rpc-worker-build/bin/rpc-server -H 0.0.0.0 -p 50052
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

BUILD_DIR="$REPO_ROOT/target/rpc-worker-build"
RPC_HOST="${RPC_HOST:-0.0.0.0}"
RPC_PORT="${RPC_PORT:-50052}"
RUN_AFTER=0
FORCE_BACKEND=""

for arg in "$@"; do
    case "$arg" in
        --run)        RUN_AFTER=1 ;;
        --backend)    : ;;                       # value consumed below
        --backend=*)  FORCE_BACKEND="${arg#*=}" ;;
        cpu|vulkan|metal) FORCE_BACKEND="$arg" ;; # bare value after --backend
        -h|--help)    sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)            echo "build-rpc-worker: unknown arg: $arg" >&2; exit 2 ;;
    esac
done

# ─── 1. Locate the pinned llama.cpp source the daemon links ──────────────
echo "==> locating pinned llama.cpp source (llama-cpp-sys-4 build output)…"
SRC="$(find "$REPO_ROOT"/target -type f -path '*llama-cpp-sys-4-*/out/llama.cpp/tools/rpc/rpc-server.cpp' 2>/dev/null \
        | head -1 | sed 's#/tools/rpc/rpc-server.cpp##')"
if [ -z "${SRC:-}" ] || [ ! -d "$SRC" ]; then
    echo "build-rpc-worker: no llama-cpp-sys-4 source found under target/." >&2
    echo "  Build the workspace first so the crate checks out llama.cpp, e.g.:" >&2
    echo "      cargo build -p sovereign-inference" >&2
    exit 1
fi
echo "    source: $SRC"
# Report the version for the host↔worker protocol-match check.
grep -m1 -rE 'define LLAMA_BUILD_NUMBER|BUILD_NUMBER' "$SRC"/*.h 2>/dev/null || true

# ─── 2. Pick the GPU backend ────────────────────────────────────────────
UNAME="$(uname -s)"
BACKEND="${FORCE_BACKEND}"
if [ -z "$BACKEND" ]; then
    case "$UNAME" in
        Linux)  BACKEND="vulkan" ;;
        Darwin) BACKEND="metal" ;;
        *)      BACKEND="cpu" ;;
    esac
fi
BACKEND_FLAGS=""
case "$BACKEND" in
    vulkan) BACKEND_FLAGS="-DGGML_VULKAN=ON" ;;
    metal)  BACKEND_FLAGS="-DGGML_METAL=ON" ;;
    cpu)    BACKEND_FLAGS="" ;;
    *)      echo "build-rpc-worker: unknown backend '$BACKEND'" >&2; exit 2 ;;
esac
echo "==> backend: $BACKEND  ($UNAME)"

command -v cmake >/dev/null 2>&1 || { echo "build-rpc-worker: cmake not found on PATH" >&2; exit 1; }

# ─── 3. The sys crate prunes tools/ui — stub it out of the tools build ──
# The crate ships a partial tree (no tools/ui, no server-common.h), so
# cmake's unconditional add_subdirectory(ui)/(server) would fail to
# configure. We only need tools/rpc, so comment those two out. Idempotent;
# operates on a regenerable build artifact under target/.
TOOLS_CMAKE="$SRC/tools/CMakeLists.txt"
# NOTE: use POSIX [[:space:]], not \s — BSD grep/sed (macOS) do not support \s.
if grep -qE '^[[:space:]]*add_subdirectory\(ui\)' "$TOOLS_CMAKE" 2>/dev/null; then
    echo "==> patching tools/CMakeLists.txt (skip pruned ui/server)…"
    sed -i.bak -E 's@^([[:space:]]*)add_subdirectory\(ui\)@\1# add_subdirectory(ui)  # rpc-worker: pruned by sys crate@' "$TOOLS_CMAKE"
    sed -i.bak -E 's@^([[:space:]]*)add_subdirectory\(server\)@\1# add_subdirectory(server)  # rpc-worker: needs pruned llama-ui@' "$TOOLS_CMAKE"
fi

# ─── 4. Configure + build rpc-server (+ llama-bench for link testing) ───
echo "==> cmake configure → $BUILD_DIR"
cmake -S "$SRC" -B "$BUILD_DIR" \
    -DCMAKE_BUILD_TYPE=Release \
    -DGGML_RPC=ON $BACKEND_FLAGS \
    -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
    >/dev/null

echo "==> building rpc-server + llama-bench…"
NPROC="$( (nproc 2>/dev/null || sysctl -n hw.ncpu) )"
cmake --build "$BUILD_DIR" --target rpc-server llama-bench -j"$NPROC"

BIN="$BUILD_DIR/bin/rpc-server"
[ -x "$BIN" ] || { echo "build-rpc-worker: rpc-server did not build" >&2; exit 1; }
echo
echo "✓ built: $BIN"
echo "  worker:  $BIN -H $RPC_HOST -p $RPC_PORT"
echo "  on the HOST (Mac), point the daemon/example at this worker:"
echo "      SOVEREIGN_RPC_WORKERS=<this-node-tailscale-ip>:$RPC_PORT  (comma-separate multiple)"
echo "      SOVEREIGN_RPC_TENSOR_SPLIT=<worker_frac>,<local_gpu_frac>  (optional; RPC first)"
echo

if [ "$RUN_AFTER" = "1" ]; then
    echo "==> exec: rpc-server -H $RPC_HOST -p $RPC_PORT"
    exec "$BIN" -H "$RPC_HOST" -p "$RPC_PORT"
fi
