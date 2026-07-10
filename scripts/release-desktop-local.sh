#!/usr/bin/env bash
# release-desktop-local.sh — cut the FULL desktop release from this one
# arm64 Mac: macOS aarch64 (native) + macOS x86_64 (cross via Rosetta
# toolchain) + Linux x86_64 (podman container), then verify every updater
# signature against the pubkey embedded in the app, and upload all assets
# to the desktop-v<version> GitHub release.
#
# First validated shipping desktop-v0.1.19 (2026-07-10, commit b440eac3).
# The per-platform traps (lance-linalg AVX-512 cfg, AppImage binfmt magic,
# virtiofs copies, DMG TCC fallback, updater second pass) are all handled
# inside the two build scripts — this driver just sequences, verifies, and
# uploads. See sovereign/crates/sovereign-desktop/RELEASING.md § "Full
# local release from the arm64 Mac".
#
# Usage:
#   scripts/release-desktop-local.sh                # everything
#   scripts/release-desktop-local.sh --skip-macos-arm --skip-macos-intel
#   scripts/release-desktop-local.sh --no-upload    # build + verify only
#   scripts/release-desktop-local.sh --upload-only  # skip builds, verify + upload what's on disk
#
# Reads from env (required unless --no-upload and you accept unsigned):
#   TAURI_SIGNING_PRIVATE_KEY / TAURI_SIGNING_PRIVATE_KEY_PASSWORD
#
# Re-run safety: every phase is idempotent. Builds are incremental,
# uploads use --clobber. If a leg fails, fix and re-run with the other
# legs --skip'd.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

log()  { printf '\n[release-desktop-local] %s\n' "$*"; }
die()  { log "ERROR: $*"; exit 1; }

SKIP_MACOS_ARM=0 SKIP_MACOS_INTEL=0 SKIP_LINUX=0 SKIP_WINDOWS=0 NO_UPLOAD=0 UPLOAD_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --skip-macos-arm)   SKIP_MACOS_ARM=1 ;;
        --skip-macos-intel) SKIP_MACOS_INTEL=1 ;;
        --skip-linux)       SKIP_LINUX=1 ;;
        --skip-windows)     SKIP_WINDOWS=1 ;;
        --no-upload)        NO_UPLOAD=1 ;;
        --upload-only)      UPLOAD_ONLY=1; SKIP_MACOS_ARM=1; SKIP_MACOS_INTEL=1; SKIP_LINUX=1; SKIP_WINDOWS=1 ;;
        *) die "unknown flag: $arg" ;;
    esac
done

# ─── Pre-flight ───────────────────────────────────────────────────────
CONF=sovereign/crates/sovereign-desktop/src-tauri/tauri.conf.json
VERSION="$(python3 -c "import json;print(json.load(open('$CONF'))['version'])")"
TAG="desktop-v$VERSION"
log "Releasing $TAG"

[[ "$(uname -sm)" == "Darwin arm64" ]] || die "this driver assumes an arm64 Mac host"

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    if (( NO_UPLOAD )); then
        log "WARNING: TAURI_SIGNING_PRIVATE_KEY not set — updater artifacts will be missing/unsigned."
    else
        die "TAURI_SIGNING_PRIVATE_KEY not set. Auto-updates NEED signed artifacts. (It normally comes from ~/.zshrc.)"
    fi
fi

# The expected minisign key ID, derived from the pubkey shipped inside the
# app — not hardcoded, so a future key rotation can't silently drift.
EXPECTED_KEY_ID="$(python3 - "$CONF" <<'EOF'
import base64, json, sys
pub = json.load(open(sys.argv[1]))["plugins"]["updater"]["pubkey"]
data = base64.b64decode(base64.b64decode(pub).decode().strip().splitlines()[1])
print(data[2:10][::-1].hex().upper())
EOF
)"
log "Updater pubkey key ID: $EXPECTED_KEY_ID"

if ! (( NO_UPLOAD )); then
    gh auth status >/dev/null 2>&1 || die "gh is not authenticated (gh auth login)"
    gh release view "$TAG" >/dev/null 2>&1 \
        || die "release $TAG does not exist. Create it first: gh release create $TAG --draft --title \"$TAG\""
fi

if ! (( SKIP_LINUX && SKIP_WINDOWS )); then
    podman machine inspect --format '{{.Resources.Memory}}' >/dev/null 2>&1 \
        || die "no podman machine. One-time setup: podman machine init --cpus 8 --memory 24576 --disk-size 120 && podman machine start"
    MEM="$(podman machine inspect --format '{{.Resources.Memory}}')"
    (( MEM >= 16384 )) || die "podman machine has ${MEM}MiB; ggml-vulkan's shader compile OOMs below ~16GiB. Resize: podman machine stop && podman machine set --memory 24576 && podman machine start"
    podman machine start >/dev/null 2>&1 || true   # no-op if already running
fi

FREE_GB="$(df -g "$REPO_ROOT" | awk 'NR==2 {print $4}')"
(( FREE_GB >= 40 )) || log "WARNING: only ${FREE_GB}GB free. A cold three-leg build wants ~40GB+; an ENOSPC mid-build corrupts the podman VM (recreate it if that happens)."

# ─── Build legs (sequential: shared cargo caches + disk headroom) ─────
(( SKIP_MACOS_ARM ))   || { log "[1/4] macOS aarch64..."; scripts/build-desktop-macos.sh --target aarch64-apple-darwin; }
(( SKIP_MACOS_INTEL )) || { log "[2/4] macOS x86_64 (cross)..."; scripts/build-desktop-macos.sh --target x86_64-apple-darwin; }
(( SKIP_LINUX ))       || { log "[3/4] Linux x86_64 (podman)..."; scripts/build-desktop-linux.sh; }
(( SKIP_WINDOWS ))     || { log "[4/4] Windows x86_64 (podman, cargo-xwin)..."; scripts/build-desktop-windows.sh; }

# ─── Collect + verify ─────────────────────────────────────────────────
MAC_ARM=target/aarch64-apple-darwin/release/bundle
MAC_X64=target/x86_64-apple-darwin/release/bundle
LINUX=target-container-linux/x86_64-unknown-linux-gnu/release/bundle
WINDOWS=target-container-windows/x86_64-pc-windows-msvc/release/bundle

ASSETS=(
    "$MAC_ARM/dmg/svrnmesh_${VERSION}_aarch64.dmg"
    "$MAC_ARM/macos/svrnmesh_${VERSION}_aarch64.app.tar.gz"
    "$MAC_ARM/macos/svrnmesh_${VERSION}_aarch64.app.tar.gz.sig"
    "$MAC_X64/dmg/svrnmesh_${VERSION}_x64.dmg"
    "$MAC_X64/macos/svrnmesh_${VERSION}_x64.app.tar.gz"
    "$MAC_X64/macos/svrnmesh_${VERSION}_x64.app.tar.gz.sig"
    "$LINUX/appimage/svrnmesh_${VERSION}_amd64.AppImage"
    "$LINUX/appimage/svrnmesh_${VERSION}_amd64.AppImage.sig"
    "$LINUX/deb/svrnmesh_${VERSION}_amd64.deb"
    "$LINUX/rpm/svrnmesh-${VERSION}-1.x86_64.rpm"
    "$WINDOWS/nsis/svrnmesh_${VERSION}_x64-setup.exe"
    "$WINDOWS/nsis/svrnmesh_${VERSION}_x64-setup.exe.sig"
)

log "Verifying assets..."
MISSING=0
for f in "${ASSETS[@]}"; do
    if [[ ! -f "$f" ]]; then log "  MISSING  $f"; MISSING=1; continue; fi
    if [[ "$f" == *.sig ]]; then
        KEY_ID="$(python3 - "$f" <<'EOF'
import base64, sys
sig = base64.b64decode(open(sys.argv[1]).read()).decode()
data = base64.b64decode(sig.strip().splitlines()[1])
print(data[2:10][::-1].hex().upper())
EOF
)"
        if [[ "$KEY_ID" != "$EXPECTED_KEY_ID" ]]; then
            die "signature key mismatch on $f: $KEY_ID != $EXPECTED_KEY_ID (wrong TAURI_SIGNING_PRIVATE_KEY?)"
        fi
        printf '  ok (sig %s)  %s\n' "$KEY_ID" "$f"
    else
        printf '  ok %9s  %s\n' "$(du -h "$f" | cut -f1)" "$f"
    fi
done
(( MISSING )) && die "assets missing — re-run the failed leg (use --skip-* for the others). Build logs explain the per-platform traps."

# ─── Upload ───────────────────────────────────────────────────────────
if (( NO_UPLOAD )); then
    log "--no-upload: stopping after verification. Upload later with: scripts/release-desktop-local.sh --upload-only"
    exit 0
fi

log "Uploading ${#ASSETS[@]} assets to $TAG..."
gh release upload "$TAG" --clobber "${ASSETS[@]}"

log "Final asset listing for $TAG:"
gh release view "$TAG" --json assets --template '{{range .assets}}  {{.name}}  {{.size}}
{{end}}'

log "Done. Smoke-test an installer, then publish the draft: gh release edit $TAG --draft=false"
