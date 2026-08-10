#!/usr/bin/env bash
# release-cli-local.sh — build + package + upload CLI tarballs for the
# same targets the desktop ships:
#
#   aarch64-apple-darwin      native            (arm64 Mac host ONLY)
#   x86_64-apple-darwin       cross             (arm64 Mac host ONLY)
#   x86_64-unknown-linux-gnu  podman container  (either host)
#
# HOST CAPABILITY. The two Apple legs need the macOS SDK (xcrun/SDKROOT) and
# cannot be built anywhere else; on an x86_64 Linux host they are SKIPPED, by
# name, and the Linux leg runs NATIVELY instead of under qemu. Both hosts
# upload into the same cli-v<version> draft, and the provenance gate below
# refuses any tarball that does not match the release being cut — so a
# two-machine release cannot mix versions or commits. The "are all the legs
# here?" question belongs to release-all.sh's publish gate, which counts
# assets on the RELEASE and therefore sees both halves. See
# scripts/lib/release-host.sh.
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

# shellcheck source=lib/release-host.sh
. "$SCRIPT_DIR/lib/release-host.sh"

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

[[ "$RELEASE_HOST_KIND" != unsupported ]] || die "$RELEASE_HOST_UNSUPPORTED_MSG"

# Auto-skip what this host cannot build — but never silently (ARCH §18.3).
# A leg that was skipped because the host lacks a toolchain reads exactly the
# same in the output as one the operator skipped on purpose, and the operator
# has to be able to tell the difference before they publish.
if ! (( RELEASE_CAN_APPLE )); then
    if ! (( SKIP_MACOS_ARM && SKIP_MACOS_INTEL )); then
        log "HOST CANNOT BUILD APPLE LEGS ($RELEASE_HOST_UNAME): skipping aarch64-apple-darwin and x86_64-apple-darwin. They need the macOS SDK — build them on the arm64 Mac and upload to the same $TAG draft."
    fi
    SKIP_MACOS_ARM=1 SKIP_MACOS_INTEL=1
fi

if ! (( NO_UPLOAD )); then
    gh auth status >/dev/null 2>&1 || die "gh is not authenticated (gh auth login)"
    gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1 \
        || die "release $TAG does not exist. Create it: gh release create $TAG --repo "$RELEASES_REPO" --draft --title \"$TAG\""
fi

# Shared with the desktop legs: SDKROOT for bindgen, deployment target, and
# the cross toolchain fragment when targeting x86_64 from arm64. Guarded on
# the Apple capability, not just on the skip flags: `xcrun` does not exist on
# Linux and this ran unconditionally, so the whole script died here before it
# could reach the Linux leg it is perfectly able to build.
if (( RELEASE_CAN_APPLE )); then
    export SDKROOT="${SDKROOT:-$(xcrun --show-sdk-path)}"
    export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-10.15}"
fi

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
    ( cd dist && release_sha256 "sovereign-$triple.tar.gz" > "sovereign-$triple.tar.gz.sha256" )
    # Provenance sidecar — what this tarball actually CONTAINS, recorded at the
    # only moment we know it for certain. The upload gate below refuses any
    # tarball whose sidecar disagrees with the release being cut. Without it a
    # stale tarball is undetectable: the filename is built from $VERSION so it
    # proves nothing, and the .sha256 is regenerated from the stale bytes, so
    # it verifies cleanly. See the desktop's equivalent gate
    # (release-desktop-local.sh, the *.app.tar.gz arm) and the incident that
    # forced it — desktop-v0.3.5 shipped a byte-identical 0.3.3 payload under a
    # 0.3.5 name, correctly signed, and users updating landed back on 0.3.3.
    cat > "dist/sovereign-$triple.tar.gz.buildinfo" <<EOF
version=$VERSION
commit=$(git rev-parse HEAD)
triple=$triple
EOF
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
    # Order matters, and it used to be wrong: `podman image exists` ran BEFORE
    # `podman machine start`, so a stopped VM failed the image probe and was
    # reported as "container image missing — run build-desktop-linux.sh". That
    # sends you off to rebuild a 3.3GB image you already have, for a machine
    # that just needed starting (observed 2026-08-08, cli-v0.5.0, 9 minutes
    # into the build). Absence of an answer is not the answer "no"
    # (ARCH §18.3): start the VM, prove we can reach it, and only then let a
    # missing-image verdict mean anything.
    release_container_ready \
        || die "cannot reach podman — $RELEASE_CONTAINER_ERR This is NOT an image problem; do not rebuild the image."
    podman image exists "$IMAGE" \
        || die "podman is reachable but image '$IMAGE' is genuinely absent — run scripts/build-desktop-linux.sh once (or its image-build step) first"
    # glslc-deadlock guard — MUST match build-desktop-linux.sh, which is why
    # both now ask the same decider (release_linux_build_cpus).
    #
    # On the arm64 Mac this leg runs --platform linux/amd64 = qemu-x86
    # emulation, where llama-cpp-sys-4's ggml-vulkan build script parallel-
    # spawns glslc sized to the visible CPU count and deadlocks at "Compiling
    # llama-cpp-sys-4" (the v0.3.0 release stalled HERE 2026-07-17 — the
    # desktop leg had this guard, the CLI leg did not, and the CLI leg runs
    # first with a COLD shader cache). taskset caps the visible CPUs so
    # hardware_concurrency — which sizes the glslc pool — sees them serial.
    #
    # On an x86_64 Linux host the SAME container is native: no emulation, no
    # missed SIGCHLD, no deadlock. Capping there would hand a 32-core box one
    # core for the longest leg of the release, so the cap is keyed to the
    # emulation, not to the platform string. Override either way with
    # SOVEREIGN_LINUX_BUILD_CPUS.
    LINUX_BUILD_CPUS="$(release_linux_build_cpus)"
    if (( RELEASE_LINUX_LEG_EMULATED )); then
        log "[linux x86_64] qemu-emulated on $RELEASE_HOST_UNAME — shader-compile concurrency capped to ${LINUX_BUILD_CPUS} CPU(s) via taskset (glslc deadlock guard)"
    else
        log "[linux x86_64] native on $RELEASE_HOST_UNAME — no emulation, so no glslc deadlock; running with ${LINUX_BUILD_CPUS} CPU(s)"
    fi
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

# ─── Provenance gate — refuse to ship what this release did not build ──
# dist/ is NOT cleaned between releases and this list is a GLOB, so whatever
# an earlier release left behind gets picked up and uploaded under today's
# tag. Nothing downstream can catch it: the filename comes from $VERSION, the
# .sha256 is regenerated from the stale bytes so it verifies, and the
# SHA256SUMS step deliberately preserves assets it did not rebuild. Observed
# 2026-08-08 cutting cli-v0.5.0 — dist/ held x86_64 mac + linux tarballs from
# Jul 29 (v0.4.x) alongside a fresh arm64 one, and only an unrelated crash
# before the upload step stopped v0.5.0 from shipping two-thirds July binaries.
#
# Four verdicts, not two (ARCH §18.1): a tarball is shippable, stale,
# unverifiable, or absent — and only the first may be uploaded.
HEAD_SHA="$(git rev-parse HEAD)"
log "Provenance gate — verifying ${#TARBALLS[@]} tarball(s) against $TAG ($VERSION @ ${HEAD_SHA:0:12})…"
for t in "${TARBALLS[@]}"; do
    info="$t.buildinfo"
    [[ -f "$info" ]] || die "$t has no .buildinfo sidecar — it predates this gate or was not built by this script. It cannot be shown to contain $VERSION, and an unverifiable payload is not shippable. Rebuild that leg, or delete $t."
    t_ver="$(sed -n 's/^version=//p' "$info")"
    t_sha="$(sed -n 's/^commit=//p' "$info")"
    if [[ "$t_ver" != "$VERSION" ]]; then
        die "$t contains version $t_ver, not $VERSION. This is a STALE artifact from an earlier release — uploading it would ship $t_ver to everyone as $VERSION, and it would verify cleanly. Rebuild that leg (dist/ is not cleaned between releases), or delete $t."
    fi
    if [[ "$t_sha" != "$HEAD_SHA" ]]; then
        die "$t was built from commit ${t_sha:0:12}, but this release is ${HEAD_SHA:0:12}. Same version number, different code — rebuild that leg, or delete $t."
    fi
    printf '  ok %9s  %s  (%s @ %s)\n' "$(du -h "$t" | cut -f1)" "$t" "$t_ver" "${t_sha:0:12}"
done

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
# `shopt -s nullglob` is in force from the TARBALLS glob above, so when the
# release carries no sidecars this expands to NOTHING and a bare `cat` reads
# STDIN — the command blocks on the terminal forever instead of failing. A
# release driver that can hang waiting on a tty is the same failure class as
# the 10.5h silent stall the watchdog was built for, so it gets a real
# verdict: found sidecars, or an error naming what is missing.
SIDECARS=("$TMP"/*.sha256)
(( ${#SIDECARS[@]} )) \
    || die "no .sha256 sidecars on $TAG after uploading ${#TARBALLS[@]} tarball(s) — SHA256SUMS would be empty. Check that the uploads above actually landed."
cat "${SIDECARS[@]}" | sort -k2 > "$TMP/SHA256SUMS"
gh release upload "$TAG" --repo "$RELEASES_REPO" --clobber "$TMP/SHA256SUMS"
rm -rf "$TMP"

log "Final asset listing for $TAG:"
gh release view "$TAG" --repo "$RELEASES_REPO" --json assets --template '{{range .assets}}  {{.name}}  {{.size}}
{{end}}'

log "Done. Smoke-test (extract a tarball, ./sovereign-cli --version → $VERSION), then publish: gh release edit $TAG --draft=false"
