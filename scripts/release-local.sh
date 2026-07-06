#!/usr/bin/env bash
# release-local.sh — cut a release from YOUR machines instead of GitHub
# Actions runners, uploading to the same draft GitHub Releases the CI
# workflows produce. Releases (storage + friend downloads) are free; only
# hosted compute costs credits — this script replaces exactly the compute.
#
# The packaging is byte-compatible with cli-release.yml, so
# landing/install.sh works unchanged: sovereign-<triple>.tar.gz containing
# sovereign-<triple>/{sovereign-cli,sovereign-cli-daemon,sovereign-cli-llm},
# a .sha256 sidecar, and a combined SHA256SUMS asset (regenerated from ALL
# uploaded sidecars after every upload, so machines can contribute their
# target in any order).
#
# Flow (run on each build machine, any order):
#   1. Once, anywhere:  scripts/bump-desktop-version.sh patch
#                       git commit … && git tag cli-vX.Y.Z desktop-vX.Y.Z
#                       git push origin main --tags
#      (the full bump script — locally the router-cache check CAN run)
#   2. Per machine:     scripts/release-local.sh cli
#                       scripts/release-local.sh desktop
#   3. Anywhere:        scripts/release-local.sh status
#      then smoke-test + Publish the drafts in the GitHub UI, as ever.
#
# Requirements: gh CLI authenticated with repo write; the repo checked out
# AT THE TAG's commit (verified below — releasing a different commit than
# the tag names is the footgun this refuses).
#
# Desktop: builds are delegated to the already-validated local scripts
# (scripts/build-desktop-{linux,macos}.sh). For working auto-updates the
# updater signing env must be set (same values as the CI `prod` secrets):
#   TAURI_UPDATER_PRIVATE_KEY + TAURI_UPDATER_PRIVATE_KEY_PASSWORD
# Without them Tauri silently skips .sig sidecars and the updater chain
# breaks — this script warns loudly but does not refuse.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

say() { printf '\033[1m[release-local]\033[0m %s\n' "$*"; }
err() { printf '\033[1;31m[release-local]\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<EOF >&2
Usage: scripts/release-local.sh <cli | desktop | status> [--no-upload]

  cli        Build + package the 3 CLI binaries for THIS machine's target,
             upload to the cli-v<version> draft release, refresh SHA256SUMS.
  desktop    Build the desktop bundle for THIS machine (delegates to
             scripts/build-desktop-<os>.sh), upload installers + .sig files
             to the desktop-v<version> draft release.
  status     Show both draft releases and their assets so far.

  --no-upload   Stop after building + packaging (inspect dist/ first).

Version + tags come from the workspace Cargo.toml; the matching tag must
exist and point at HEAD (run the bump/tag/push flow first — see header).
EOF
    exit 2
}

# ── Shared resolution ─────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null 2>&1 || err "'$1' is required"; }

# Same dual-tool dance as landing/install.sh: shasum (mac/ubuntu) or
# coreutils sha256sum (fedora). Both emit "hash  filename"; install.sh
# compares only the hash field.
sha256_line() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1"
    elif command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"
    else err "need shasum or sha256sum"
    fi
}

resolve_common() {
    need git; need tar
    VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
    [ -n "$VERSION" ] || err "cannot read version from Cargo.toml"
    TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
    [ -n "$TRIPLE" ] || err "cannot resolve host target triple (is rustc installed?)"
}

# gh is only needed to talk to GitHub — building/packaging (--no-upload)
# works without it (e.g. a build box that hands artifacts to another
# machine for upload).
need_gh() {
    need gh
    gh auth status >/dev/null 2>&1 || err "gh is not authenticated (run: gh auth login)"
}

# The tag must exist and name the commit we are about to build. Releasing
# HEAD while the tag points elsewhere ships bits the tag doesn't describe.
verify_tag_at_head() {
    local tag="$1"
    git rev-parse -q --verify "refs/tags/$tag" >/dev/null \
        || err "tag '$tag' does not exist — run the bump/tag/push flow first (scripts/bump-desktop-version.sh prints the commands)"
    local tag_sha head_sha
    tag_sha="$(git rev-parse "$tag^{commit}")"
    head_sha="$(git rev-parse HEAD)"
    [ "$tag_sha" = "$head_sha" ] \
        || err "tag '$tag' points at ${tag_sha:0:12} but HEAD is ${head_sha:0:12} — check out the tag (git checkout $tag) or retag"
    if [ -n "$(git status --porcelain)" ]; then
        say "WARNING: working tree is dirty — the build will include uncommitted changes."
    fi
}

# Create the draft release if it doesn't exist yet (idempotent across the
# machines that share it).
ensure_draft_release() {
    local tag="$1" title="$2"; shift 2
    if gh release view "$tag" >/dev/null 2>&1; then
        say "release $tag already exists — appending assets"
    else
        say "creating draft release $tag"
        gh release create "$tag" --draft --title "$title" "$@" \
            || gh release view "$tag" >/dev/null 2>&1 \
            || err "could not create or find release $tag"
    fi
}

# ── cli ───────────────────────────────────────────────────────────────────

cmd_cli() {
    local no_upload="$1"
    resolve_common
    local tag="cli-v$VERSION"
    verify_tag_at_head "$tag"

    say "building CLI v$VERSION for $TRIPLE (true release profile — this is the slow, shipping build)"
    # Mirror cli-release.yml exactly: --locked, explicit --target (also keeps
    # this tree separate from dev-release.sh's env-tweaked target/release).
    cargo build --release --locked --target "$TRIPLE" \
        -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-llm

    local bindir="target/$TRIPLE/release"
    local stage="dist/sovereign-$TRIPLE"
    rm -rf "$stage"; mkdir -p "$stage"
    for b in sovereign-cli sovereign-cli-daemon sovereign-cli-llm; do
        cp "$bindir/$b" "$stage/"
        strip "$stage/$b" || true # best-effort; trims llama.cpp debug info
    done
    tar -czf "dist/sovereign-$TRIPLE.tar.gz" -C dist "sovereign-$TRIPLE"
    ( cd dist && sha256_line "sovereign-$TRIPLE.tar.gz" > "sovereign-$TRIPLE.tar.gz.sha256" )
    say "packaged: dist/sovereign-$TRIPLE.tar.gz"

    [ "$no_upload" = "1" ] && { say "--no-upload: stopping after packaging"; return 0; }

    need_gh
    ensure_draft_release "$tag" "$tag" --generate-notes
    say "uploading tarball + checksum"
    gh release upload "$tag" --clobber \
        "dist/sovereign-$TRIPLE.tar.gz" \
        "dist/sovereign-$TRIPLE.tar.gz.sha256"

    # Rebuild SHA256SUMS from EVERY sidecar currently on the release, so it
    # stays correct no matter which machine uploaded last. (install.sh
    # verifies the tarball against this file.)
    say "refreshing combined SHA256SUMS"
    local sums_dir; sums_dir="$(mktemp -d)"
    gh release download "$tag" --pattern '*.sha256' --dir "$sums_dir"
    cat "$sums_dir"/*.sha256 > "$sums_dir/SHA256SUMS"
    gh release upload "$tag" --clobber "$sums_dir/SHA256SUMS"
    rm -rf "$sums_dir"

    say "done — $tag now carries $TRIPLE. Run 'scripts/release-local.sh status' to see all assets."
}

# ── desktop ───────────────────────────────────────────────────────────────

cmd_desktop() {
    local no_upload="$1"
    resolve_common
    local tag="desktop-v$VERSION"
    verify_tag_at_head "$tag"

    if [ -z "${TAURI_UPDATER_PRIVATE_KEY:-}" ]; then
        say "WARNING: TAURI_UPDATER_PRIVATE_KEY is not set — Tauri will SILENTLY skip"
        say "         the .sig updater sidecars and in-app auto-update will 404 for"
        say "         this release. Export the key (+ _PASSWORD) to fix, or proceed"
        say "         for a no-updater release."
    fi

    local os build_script
    os="$(uname -s)"
    case "$os" in
        Darwin) build_script="scripts/build-desktop-macos.sh" ;;
        Linux)  build_script="scripts/build-desktop-linux.sh" ;;
        *) err "unsupported desktop build host: $os" ;;
    esac
    say "building desktop v$VERSION via $build_script (the CI-mirroring local build)"
    "$build_script"

    # Collect the same asset set desktop-release.yml publishes. Bundles land
    # under the target-triple release dir (tauri) — search both the explicit
    # triple tree and the plain release tree to cover either build style.
    local assets=()
    while IFS= read -r f; do assets+=("$f"); done < <(
        find "target/$TRIPLE/release/bundle" target/release/bundle -type f \
            \( -name "*.dmg" -o -name "*.AppImage" -o -name "*.deb" \
               -o -name "*.msi" -o -name "*.exe" -o -name "*.app.tar.gz" \
               -o -name "*.sig" \) 2>/dev/null | sort -u
    )
    [ "${#assets[@]}" -gt 0 ] || err "no desktop bundle artifacts found under target/**/release/bundle — did the build succeed?"
    say "found ${#assets[@]} artifact(s):"; printf '  %s\n' "${assets[@]}"

    [ "$no_upload" = "1" ] && { say "--no-upload: stopping after build"; return 0; }

    need_gh
    ensure_draft_release "$tag" "Sovereign Desktop $tag" --generate-notes --notes "Unsigned MVP release. macOS users: right-click → Open the first time (Gatekeeper will warn). Windows users: SmartScreen will warn — click \"More info\" → \"Run anyway\"."
    say "uploading ${#assets[@]} asset(s)"
    gh release upload "$tag" --clobber "${assets[@]}"
    say "done — $tag now carries this machine's installers."
}

# ── status ────────────────────────────────────────────────────────────────

cmd_status() {
    resolve_common
    need_gh
    for tag in "cli-v$VERSION" "desktop-v$VERSION"; do
        echo
        if gh release view "$tag" --json name,isDraft,assets \
            --template '{{.name}}  (draft: {{.isDraft}})
{{range .assets}}  {{.name}}  {{.size}} bytes
{{end}}' 2>/dev/null; then :; else
            echo "$tag: no release yet"
        fi
    done
}

# ── dispatch ──────────────────────────────────────────────────────────────

[ $# -ge 1 ] || usage
cmd="$1"; shift
no_upload=0
for a in "$@"; do
    case "$a" in
        --no-upload) no_upload=1 ;;
        *) usage ;;
    esac
done
case "$cmd" in
    cli)     cmd_cli "$no_upload" ;;
    desktop) cmd_desktop "$no_upload" ;;
    status)  cmd_status ;;
    *) usage ;;
esac
