#!/usr/bin/env bash
# release-cli-local.sh — build + package + upload CLI tarballs for the
# same targets the desktop ships, all from this one arm64 Mac:
#
#   aarch64-apple-darwin      native
#   x86_64-apple-darwin       cross (same toolchain fixes as the desktop:
#                             CMAKE_TOOLCHAIN_FILE fragment + vendored
#                             lance-linalg — see commit b440eac3)
#   x86_64-unknown-linux-gnu  podman container (reuses the desktop build
#                             image + .cargo-container / target-container-linux
#                             caches, so it's warm after a desktop release)
#
# Packaging follows .github/workflows/cli-release.yml exactly: the three
# binaries (sovereign-cli, sovereign-cli-daemon, sovereign-cli-llm) built
# --release --locked, staged under dist/sovereign-<triple>/, stripped,
# tarred, sha256'd. SHA256SUMS is regenerated from ALL sidecars on the
# release (existing CI-built assets included) so mixed CI/local releases
# stay consistent.
#
# Usage:
#   scripts/release-cli-local.sh                    # all three legs + upload
#   scripts/release-cli-local.sh --skip-macos-arm --skip-linux   # one leg
#   scripts/release-cli-local.sh --no-upload        # build + package only
#   scripts/release-cli-local.sh --upload-only      # push what's in dist/
#
# The cli-v<version> draft release must already exist (CI creates it on
# tag push; otherwise: gh release create cli-v<ver> --draft).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

log() { printf '\n[release-cli-local] %s\n' "$*"; }
die() { log "ERROR: $*"; exit 1; }

SKIP_MACOS_ARM=0 SKIP_MACOS_INTEL=0 SKIP_LINUX=0 NO_UPLOAD=0 UPLOAD_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --skip-macos-arm)   SKIP_MACOS_ARM=1 ;;
        --skip-macos-intel) SKIP_MACOS_INTEL=1 ;;
        --skip-linux)       SKIP_LINUX=1 ;;
        --no-upload)        NO_UPLOAD=1 ;;
        --upload-only)      UPLOAD_ONLY=1; SKIP_MACOS_ARM=1; SKIP_MACOS_INTEL=1; SKIP_LINUX=1 ;;
        *) die "unknown flag: $arg" ;;
    esac
done

BINS=(sovereign-cli sovereign-cli-daemon sovereign-cli-llm)

# ─── Pre-flight ───────────────────────────────────────────────────────
VERSION="$(python3 -c "
import tomllib
print(tomllib.load(open('Cargo.toml','rb'))['workspace']['package']['version'])")"
TAG="cli-v$VERSION"
# Releases publish to the PUBLIC shelf repo, not the (invite-only) source
# repo: assets on a private repo aren't anonymously fetchable, which breaks
# install.sh, the landing-page downloads, and the desktop auto-updater.
# Override for testing with RELEASES_REPO.
RELEASES_REPO="${RELEASES_REPO:-alexsbryan/svrnmesh-releases}"
log "Releasing $TAG (workspace version $VERSION)"

[[ "$(uname -sm)" == "Darwin arm64" ]] || die "this driver assumes an arm64 Mac host"

if ! (( NO_UPLOAD )); then
    gh auth status >/dev/null 2>&1 || die "gh is not authenticated (gh auth login)"
    gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1 \
        || die "release $TAG does not exist. Create it: gh release create $TAG --repo "$RELEASES_REPO" --draft --title \"$TAG\""
fi

# Shared with the desktop legs: SDKROOT for bindgen, deployment target,
# and the cross toolchain fragment when targeting x86_64 from arm64.
export SDKROOT="${SDKROOT:-$(xcrun --show-sdk-path)}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-10.15}"

package() {  # package <triple>  — stage + strip + tar + sha256, CI-identical
    local triple="$1" bindir="$2"
    local stage="dist/sovereign-$triple"
    rm -rf "$stage"
    mkdir -p "$stage"
    for b in "${BINS[@]}"; do
        cp "$bindir/$b" "$stage/"
        strip "$stage/$b" 2>/dev/null || true
    done
    tar -czf "dist/sovereign-$triple.tar.gz" -C dist "sovereign-$triple"
    ( cd dist && shasum -a 256 "sovereign-$triple.tar.gz" > "sovereign-$triple.tar.gz.sha256" )
    log "packaged dist/sovereign-$triple.tar.gz ($(du -h "dist/sovereign-$triple.tar.gz" | cut -f1))"
}

# ─── macOS legs ───────────────────────────────────────────────────────
build_mac() {  # build_mac <triple>
    local triple="$1"
    rustup target list --installed | grep -q "^$triple$" || rustup target add "$triple"
    # No per-triple special case. There used to be an x86_64-apple-darwin arm
    # that exported CMAKE_TOOLCHAIN_FILE=scripts/cmake/darwin-cross-no-openssl.cmake
    # to defeat llama.cpp's LLAMA_OPENSSL=ON finding the HOST (arm64) Homebrew
    # OpenSSL. It never worked: the assignment ended in a `\` line-continuation
    # followed by a COMMENT line, so bash joined them into `VAR=… # …` — a bare,
    # unexported assignment — and the `cargo build` on the next line ran as a
    # SEPARATE command that never saw the variable. Both branches were byte-
    # identical in effect, and the Intel leg died at link with
    #     "_X509_verify_cert_error_string" … ld: symbol(s) not found for x86_64
    # (cli-v0.5.0, 2026-08-08). LLAMA_OPENSSL is now forced OFF unconditionally
    # in vendor/llama-cpp-sys-4/build.rs, which is the single decider for every
    # target and consumer and cannot be silently disabled by shell quoting.
    #
    # `code-intel` is REQUIRED in the shipped build. Without it `svrn code
    # index` is not in the binary — which is the exact defect this flag was
    # added to fix (2026-08-06): `code` was a dev verb, the sibling that
    # served it was never packaged, and `svrn doctor` told users to run it
    # anyway. It adds corpus-engine's grammars + the SCIP db + a pure-HTTP
    # embed client; no llama.cpp, no LanceDB.
    cargo build --release --locked --target "$triple" \
        --features sovereign-cli/code-intel \
        -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm
    package "$triple" "target/$triple/release"
}

(( SKIP_MACOS_ARM ))   || { log "[mac aarch64] building..."; build_mac aarch64-apple-darwin; }
(( SKIP_MACOS_INTEL )) || { log "[mac x86_64] cross-building..."; build_mac x86_64-apple-darwin; }

# ─── Linux leg (podman) ───────────────────────────────────────────────
if ! (( SKIP_LINUX )); then
    log "[linux x86_64] building in container..."
    IMAGE="sovereign-desktop-linux-build:latest"
    podman image exists "$IMAGE" \
        || die "container image missing — run scripts/build-desktop-linux.sh once (or its image-build step) first"
    podman machine start >/dev/null 2>&1 || true
    # qemu-x86 glslc-deadlock guard — MUST match build-desktop-linux.sh. This leg
    # runs --platform linux/amd64 = qemu-x86 emulation on the arm64 host, where
    # llama-cpp-sys-4's ggml-vulkan build script parallel-spawns glslc sized to the
    # visible CPU count and deadlocks at "Compiling llama-cpp-sys-4" (v0.3.0 release
    # stalled HERE 2026-07-17 — the desktop leg had this guard, the CLI leg did not,
    # and the CLI leg runs first with a COLD shader cache). taskset caps the visible
    # CPUs so hardware_concurrency — which sizes the glslc pool — sees them serial.
    # Override with SOVEREIGN_LINUX_BUILD_CPUS (>= host nproc disables it; warm caches only).
    LINUX_BUILD_CPUS="${SOVEREIGN_LINUX_BUILD_CPUS:-1}"
    log "[linux x86_64] shader-compile concurrency capped to ${LINUX_BUILD_CPUS} CPU(s) via taskset (qemu glslc deadlock guard)"
    # Same mounts as the desktop build → same warm caches. The image's env
    # already points CARGO_TARGET_DIR/CARGO_HOME at the /work-side caches.
    podman run --rm --platform linux/amd64 \
        -v "$REPO_ROOT:/work:Z" \
        --entrypoint /bin/bash "$IMAGE" -c \
        "cd /work && taskset -c 0-$((LINUX_BUILD_CPUS - 1)) cargo build --release --locked --target x86_64-unknown-linux-gnu \
             --features sovereign-cli/code-intel \
             -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm"
    package x86_64-unknown-linux-gnu "target-container-linux/x86_64-unknown-linux-gnu/release"
fi

# ─── Upload + SHA256SUMS ──────────────────────────────────────────────
if (( NO_UPLOAD )); then
    log "--no-upload: tarballs are in dist/. Push later with --upload-only."
    exit 0
fi

shopt -s nullglob
TARBALLS=(dist/sovereign-*.tar.gz)
(( ${#TARBALLS[@]} )) || die "nothing in dist/ to upload"

log "Uploading ${#TARBALLS[@]} tarball(s) + sidecars to $TAG..."
for t in "${TARBALLS[@]}"; do
    gh release upload "$TAG" --repo "$RELEASES_REPO" --clobber "$t" "$t.sha256"
done

# Regenerate SHA256SUMS from every .sha256 sidecar ON THE RELEASE, so
# CI-built assets we didn't rebuild locally stay covered.
log "Regenerating SHA256SUMS from all release sidecars..."
TMP="$(mktemp -d)"
for name in $(gh release view "$TAG" --repo "$RELEASES_REPO" --json assets --template '{{range .assets}}{{.name}}
{{end}}' | grep '\.tar\.gz\.sha256$'); do
    gh release download "$TAG" --repo "$RELEASES_REPO" --pattern "$name" --dir "$TMP" --clobber
done
cat "$TMP"/*.sha256 | sort -k2 > "$TMP/SHA256SUMS"
gh release upload "$TAG" --repo "$RELEASES_REPO" --clobber "$TMP/SHA256SUMS"
rm -rf "$TMP"

log "Final asset listing for $TAG:"
gh release view "$TAG" --repo "$RELEASES_REPO" --json assets --template '{{range .assets}}  {{.name}}  {{.size}}
{{end}}'

log "Done. Smoke-test (extract a tarball, ./sovereign-cli --version → $VERSION), then publish: gh release edit $TAG --draft=false"
