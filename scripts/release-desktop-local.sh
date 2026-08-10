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

# shellcheck source=lib/release-host.sh
. "$SCRIPT_DIR/lib/release-host.sh"

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
# Releases publish to the PUBLIC shelf repo, not the (invite-only) source
# repo: assets on a private repo aren't anonymously fetchable, which breaks
# install.sh, the landing-page downloads, and the desktop auto-updater.
# Override for testing with RELEASES_REPO.
RELEASES_REPO="${RELEASES_REPO:-alexsbryan/svrnmesh-releases}"
log "Releasing $TAG"

[[ "$RELEASE_HOST_KIND" != unsupported ]] || die "$RELEASE_HOST_UNSUPPORTED_MSG"

# Auto-skip what this host cannot build, by name (ARCH §18.3). The macOS
# bundler needs the SDK, codesign, hdiutil and plutil; none exist on Linux.
# The asset manifest below is narrowed to match, so a Linux run verifies and
# uploads its own legs instead of reporting the Apple ones MISSING.
if ! (( RELEASE_CAN_APPLE )); then
    if ! (( SKIP_MACOS_ARM && SKIP_MACOS_INTEL )); then
        log "HOST CANNOT BUILD APPLE LEGS ($RELEASE_HOST_UNAME): skipping macOS aarch64 and x86_64. Build them on the arm64 Mac and upload to the same $TAG draft; release-all.sh's publish gate counts assets on the release and will refuse to publish until both halves have landed."
    fi
    SKIP_MACOS_ARM=1 SKIP_MACOS_INTEL=1
fi

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
    gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1 \
        || die "release $TAG does not exist. Create it first: gh release create $TAG --repo "$RELEASES_REPO" --draft --title \"$TAG\""
fi

if ! (( SKIP_LINUX && SKIP_WINDOWS )); then
    release_container_ready || die "$RELEASE_CONTAINER_ERR"
fi

FREE_GB="$(release_free_gb "$REPO_ROOT")"
(( FREE_GB >= 40 )) || log "WARNING: only ${FREE_GB}GB free. A cold three-leg build wants ~40GB+; an ENOSPC mid-build corrupts the podman VM (recreate it if that happens)."

# Router-embed cache freshness. desktop-release.yml has a CI gate for this;
# local releases bypass the workflow entirely (v0.1.19 shipped ungated —
# harmless that time, but only by luck). A stale baked cache means every
# fresh install re-embeds ~303 router exemplars at first launch: minutes on
# a CPU-only embed slot. Exit 3 = stale.
if ! (( UPLOAD_ONLY )); then
    # This is a NATIVE cargo build, not a container one. On the Fedora host
    # that matters: llama-cpp-sys-4 needs clang + Vulkan headers, which live
    # in the sovereign-vulkan toolbox, so release_native_run re-enters it.
    log "Checking router-embed cache freshness (native cargo, via $RELEASE_NATIVE_RUN_VIA)..."
    set +e
    release_native_run cargo run --quiet --release -p sovereign-cli-llm -- router-cache check
    ROUTER_RC=$?
    set -e
    case "$ROUTER_RC" in
        0) ;;
        3) die "router-embed cache is STALE — run: cargo run --release -p sovereign-cli-llm -- router-cache rebuild, commit sovereign/router/router-embed-cache.json, and re-run" ;;
        *) die "router-cache check errored (exit $ROUTER_RC) — fix before releasing" ;;
    esac
fi

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

# The FULL release manifest is 12 assets. This list is deliberately NOT
# narrowed by the --skip-* flags: --upload-only skips every leg and must
# still verify all twelve off disk, and a partial rebuild must not be able to
# publish a release that silently lost an asset.
#
# It IS narrowed by host CAPABILITY, which is a different thing. The six
# Apple assets will never exist on a Linux disk no matter how many times you
# re-run, so listing them there would only produce six MISSING lines and a
# refusal. Completeness for a two-machine release is enforced where both
# halves are visible: release-all.sh's publish gate counts assets on the
# GitHub release and refuses to flip the draft below 12.
ASSETS=()
if (( RELEASE_CAN_APPLE )); then
    ASSETS+=(
        "$MAC_ARM/dmg/svrnmesh_${VERSION}_aarch64.dmg"
        "$MAC_ARM/macos/svrnmesh_${VERSION}_aarch64.app.tar.gz"
        "$MAC_ARM/macos/svrnmesh_${VERSION}_aarch64.app.tar.gz.sig"
        "$MAC_X64/dmg/svrnmesh_${VERSION}_x64.dmg"
        "$MAC_X64/macos/svrnmesh_${VERSION}_x64.app.tar.gz"
        "$MAC_X64/macos/svrnmesh_${VERSION}_x64.app.tar.gz.sig"
    )
fi
ASSETS+=(
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
    elif [[ "$f" == *.app.tar.gz ]]; then
        # Assert the PAYLOAD is the version we are shipping — the filename is
        # constructed from $VERSION, so it proves nothing on its own.
        #
        # desktop-v0.3.5 (2026-07-27) shipped a byte-identical copy of the
        # 0.3.3 payload under a 0.3.5 name. Every check here passed: the file
        # existed, and the signature was valid — because the .sig signs BYTES,
        # and those bytes genuinely were a correctly-signed 0.3.3 build. Users
        # who updated landed back on 0.3.3 and the update prompt never cleared.
        # The build script now refuses to create such a file; this is the
        # independent gate, and the only one that runs under --upload-only.
        TAR_VERSION="$(tar -xzOf "$f" '*.app/Contents/Info.plist' 2>/dev/null \
            | plutil -extract CFBundleShortVersionString raw -o - - 2>/dev/null || true)"
        if [[ -z "$TAR_VERSION" ]]; then
            die "cannot read the app version inside $f — refusing to upload an unverifiable payload."
        fi
        if [[ "$TAR_VERSION" != "$VERSION" ]]; then
            die "$f contains version $TAR_VERSION, not $VERSION. This is a STALE artifact from an earlier build — uploading it would ship $TAR_VERSION to everyone as $VERSION, and it would verify cleanly. Rebuild that leg (the target dir was not cleaned between releases)."
        fi
        printf '  ok %9s  %s  (payload %s)\n' "$(du -h "$f" | cut -f1)" "$f" "$TAR_VERSION"
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
gh release upload "$TAG" --repo "$RELEASES_REPO" --clobber "${ASSETS[@]}"

log "Final asset listing for $TAG:"
gh release view "$TAG" --repo "$RELEASES_REPO" --json assets --template '{{range .assets}}  {{.name}}  {{.size}}
{{end}}'

log "Done. Smoke-test an installer, then publish the draft: gh release edit $TAG --draft=false"
