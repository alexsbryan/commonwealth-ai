#!/usr/bin/env bash
# stage-daemon-sidecar.sh — build `sovereign-cli-daemon` and the loader it execs
# (`sovereign-stock`), and stage both where
# Tauri's `externalBin` expects to find them, so a release installer carries a
# daemon and a machine with no CLI on PATH still gets a working app (svt-1,
# sv-surface `sv-no-daemon-management`).
#
# Usage:
#   scripts/stage-daemon-sidecar.sh [<target_triple>]
#
# Companion to scripts/fetch-desktop-binaries.sh, which stages the OCR assets
# into the same directory under the same `binaries/<name>-<triple>` naming.
# They are SEPARATE scripts because they are separate acts and, decisively,
# they must run at different moments: the OCR assets are third-party
# DOWNLOADS with no toolchain prerequisite, so the fetch runs early; this one
# invokes cargo and therefore has to run AFTER each build path has finished
# setting its toolchain up (the Windows leg, for instance, writes its
# cargo-xwin CXXFLAGS/CMake env only just before the build). Folding this
# into the fetch script would have put a cargo build ahead of the environment
# it needs. `SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH` is likewise not honoured
# here on purpose — skipping a re-download is sensible, skipping a rebuild
# after a code change would stage a stale daemon.
#
# Env:
#   SOVEREIGN_DESKTOP_CARGO_RUNNER   cargo command to use (default `cargo`);
#                                    the Windows leg sets `cargo-xwin`.
#   SOVEREIGN_DESKTOP_SIDECAR_PROFILE  `release` (default) or `debug`.
#   SOVEREIGN_SIDECAR_FEATURES       comma-separated cargo features for the
#                                    LOADER build (`sovereign-stock`, staged
#                                    beside the sidecar). This is where the
#                                    WINDOWS GPU BACKEND is selected —
#                                    `windows-vulkan` or `windows-cuda`. They
#                                    were `sovereign-desktop` features until
#                                    sv-surface svt-7 (2026-09-12), then
#                                    `sovereign-cli-daemon`'s until
#                                    pb-distribution-setup; the process that
#                                    loads the weights is the stock binary, so
#                                    the backend is chosen on ITS build.
#   CARGO_TARGET_DIR                 honoured; the container legs set it.
#
# Why release by default: this stages an artifact that ships inside an
# installer, and `cargo tauri build` is itself a release build. A debug
# daemon is ~3x the size and materially slower; `--debug` exists only so a
# local rehearsal of the packaging can skip the release compile.
#
# Cost note (the svt-1 kill bar). This does NOT introduce a daemon build into
# the desktop's CI job: `sovereign-desktop/src-tauri/Cargo.toml` already
# path-depends on `sovereign-cli-daemon` (the `--daemon-child` re-exec), so
# the crate and its whole dependency tree are compiled by the desktop build
# either way. What this adds is the `[[bin]]` link step on top of artifacts
# the job already has, into the same CARGO_TARGET_DIR.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
DESKTOP_BIN_DIR="${REPO_ROOT}/clients/desktop/src-tauri/binaries"

# Must equal `daemon_binary::SIDECAR_BINARY` and the `externalBin` entry in
# tauri.release.conf.json. A Rust test asserts the latter two agree
# (`daemon_binary::tests::packaging`); this script's own agreement is checked
# at the end, where a missing staged file is a hard failure rather than a
# build that quietly ships no daemon.
SIDECAR_NAME="sovereign-cli-daemon"
# Must equal `daemon_binary::LOADER_SIDECAR`: the stock binary the sidecar
# execs for `daemon run` and setup's probe, found beside it
# (pb-distribution-setup). The same packaging test pins it.
LOADER_NAME="sovereign-stock"
CARGO_RUNNER="${SOVEREIGN_DESKTOP_CARGO_RUNNER:-cargo}"
PROFILE="${SOVEREIGN_DESKTOP_SIDECAR_PROFILE:-release}"

# ─── Target detection (same contract as fetch-desktop-binaries.sh) ───

target_triple_arg="${1:-}"
if [[ -n "$target_triple_arg" ]]; then
    TARGET="$target_triple_arg"
else
    if ! command -v rustc >/dev/null 2>&1; then
        echo "stage-daemon-sidecar: rustc not on PATH; pass a target triple explicitly" >&2
        exit 2
    fi
    TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
fi

case "$TARGET" in
    *-pc-windows-*) EXE_SUFFIX=".exe" ;;
    *)              EXE_SUFFIX="" ;;
esac

echo "stage-daemon-sidecar: target=$TARGET profile=$PROFILE runner=$CARGO_RUNNER"
mkdir -p "$DESKTOP_BIN_DIR"

# Concurrent agents serialize on the cargo package lock (AGENTS.md
# "Compilation and test feedback"). The wrapper is a no-cost pass-through for
# a solo build machine — one mkdir — and the difference between a queue and
# two builds idling on each other when it is not.
LOCK="$REPO_ROOT/scripts/with-cargo-lock.sh"
if [[ -x "$LOCK" ]]; then
    RUN=("$LOCK" "$CARGO_RUNNER")
else
    RUN=("$CARGO_RUNNER")
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"

# stage <package> <bin> [<features>] — build one binary and stage it under
# the per-triple name `externalBin` expects.
stage() {
    local pkg="$1" bin="$2" features="${3:-}"
    local dest="$DESKTOP_BIN_DIR/${bin}-${TARGET}${EXE_SUFFIX}"
    local args=(build --target "$TARGET" -p "$pkg" --bin "$bin")
    [[ "$PROFILE" == "release" ]] && args+=(--release)
    if [[ -n "$features" ]]; then
        args+=(--features "$features")
        echo "stage-daemon-sidecar: $bin features=$features"
    fi
    echo "stage-daemon-sidecar: dest=$dest"
    echo "stage-daemon-sidecar: ${RUN[*]} ${args[*]}"
    if ! ( cd "$REPO_ROOT" && "${RUN[@]}" "${args[@]}" ); then
        echo "stage-daemon-sidecar: $bin build FAILED — refusing to stage a stale or absent binary" >&2
        exit 1
    fi

    local built="$TARGET_DIR/$TARGET/$PROFILE/${bin}${EXE_SUFFIX}"
    if [[ ! -f "$built" ]]; then
        echo "stage-daemon-sidecar: the build reported success but $built does not exist." >&2
        echo "  CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-<unset, using $REPO_ROOT/target>}" >&2
        exit 1
    fi

    # `cp` over a previously-staged file whose mode came from a read-only
    # source fails with EACCES; remove first (the same trap
    # build-desktop-macos.sh's tesseract staging hit).
    rm -f "$dest"
    if ! cp "$built" "$dest"; then
        echo "stage-daemon-sidecar: cp $built -> $dest failed" >&2
        exit 1
    fi
    chmod +x "$dest"
    local size
    size="$(wc -c < "$dest" | tr -d ' ')"
    echo "stage-daemon-sidecar: staged $(basename "$dest") (${size} bytes)"
}

# The sidecar loads no model, so it takes no features. The loader it execs
# (`daemon run`, setup's probe) is the build the GPU backend is chosen on.
stage sovereign-cli-daemon "$SIDECAR_NAME"
stage sovereign-stock "$LOADER_NAME" "${SOVEREIGN_SIDECAR_FEATURES:-}"
echo
echo "Next:"
echo "  cd clients/desktop"
echo "  cargo tauri build --config src-tauri/tauri.release.conf.json"
