#!/usr/bin/env bash
# release-vsix-local.sh — build + package + upload the VS Code extension
# (`packages/vscode-sovereign`) to the public shelf repo.
#
# The extension is pure TypeScript bundled by esbuild — one platform-neutral
# .vsix, no cross-compilation, no containers. So unlike release-cli-local.sh
# and release-desktop-local.sh there is no CI pipeline to mirror: this script
# IS the release path.
#
# The extension carries its OWN version (packages/vscode-sovereign/package.json)
# and is NOT pinned to the workspace version — it ships on its own cadence.
# Tag is vscode-v<that version>.
#
# Usage:
#   scripts/release-vsix-local.sh              # build + package + upload draft
#   scripts/release-vsix-local.sh --no-upload  # build + package only
#   scripts/release-vsix-local.sh --publish    # also flip the draft public
#
# Publishing pins --latest=false on purpose: the shelf's "Latest" badge is the
# front door for humans, and it should keep pointing at the desktop app rather
# than following whichever artifact shipped most recently.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
EXT_DIR="$REPO_ROOT/packages/vscode-sovereign"

log() { printf '\n[release-vsix-local] %s\n' "$*"; }
die() { log "ERROR: $*"; exit 1; }

NO_UPLOAD=0 PUBLISH=0
for arg in "$@"; do
    case "$arg" in
        --no-upload) NO_UPLOAD=1 ;;
        --publish)   PUBLISH=1 ;;
        *) die "unknown flag: $arg" ;;
    esac
done

cd "$EXT_DIR"

# ─── Pre-flight ───────────────────────────────────────────────────────
VERSION="$(node -p "require('./package.json').version")"
TAG="vscode-v$VERSION"
# Same shelf the CLI and desktop publish to — assets on the private source
# repo aren't anonymously fetchable, which is the whole point of the split.
RELEASES_REPO="${RELEASES_REPO:-alexsbryan/svrnmesh-releases}"
VSIX="sovereign-fim-$VERSION.vsix"
log "Releasing $TAG (extension version $VERSION) to $RELEASES_REPO"

if ! (( NO_UPLOAD )); then
    gh auth status >/dev/null 2>&1 || die "gh is not authenticated (gh auth login)"
fi

# ─── Build ────────────────────────────────────────────────────────────
# LICENSE is a copy of the repo-root AGPL text. vsce warns (not errors) when
# it's absent, and a publicly downloadable artifact that declares
# AGPL-3.0-or-later in its manifest must carry the text it points at.
[[ -f LICENSE ]] || cp "$REPO_ROOT/LICENSE" LICENSE

log "installing deps..."
npm install --silent

log "running tests..."
npm test

log "packaging..."
npm run package
[[ -f "$VSIX" ]] || die "expected $VSIX, not found (version mismatch?)"

shasum -a 256 "$VSIX" > "$VSIX.sha256"
log "packaged $VSIX ($(du -h "$VSIX" | cut -f1))"

# Smoke it before it leaves the machine: a .vsix that won't install is the
# one failure a checksum can't catch.
if command -v code >/dev/null 2>&1; then
    log "smoke-installing into VS Code..."
    code --install-extension "$PWD/$VSIX" --force >/dev/null \
        || die "the packaged .vsix failed to install locally — do not ship it"
    log "smoke install OK"
else
    log "WARNING: no 'code' CLI on PATH — skipping the install smoke test"
fi

if (( NO_UPLOAD )); then
    log "--no-upload: $VSIX is in $EXT_DIR. Re-run without the flag to push."
    exit 0
fi

# ─── Upload ───────────────────────────────────────────────────────────
if gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1; then
    log "release $TAG exists — uploading assets with --clobber"
    gh release upload "$TAG" --repo "$RELEASES_REPO" --clobber "$VSIX" "$VSIX.sha256"
else
    log "creating draft release $TAG"
    gh release create "$TAG" --repo "$RELEASES_REPO" \
        --title "svrn fim (VS Code) v$VERSION" \
        --notes "svrn fim $VERSION — local-first inline completion for VS Code. Requires a svrn daemon with a FIM slot: \`svrn setup --fim\`." \
        --latest=false --draft \
        "$VSIX" "$VSIX.sha256"
fi

if (( PUBLISH )); then
    log "publishing $TAG (keeping the Latest badge on the desktop app)"
    gh release edit "$TAG" --repo "$RELEASES_REPO" --draft=false --latest=false

    log "verifying the anonymous download path..."
    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT
    ( cd "$TMP" \
      && env -u GITHUB_TOKEN -u GH_TOKEN curl -fsSL -O \
           "https://github.com/$RELEASES_REPO/releases/download/$TAG/$VSIX" \
      && env -u GITHUB_TOKEN -u GH_TOKEN curl -fsSL -O \
           "https://github.com/$RELEASES_REPO/releases/download/$TAG/$VSIX.sha256" \
      && shasum -a 256 -c "$VSIX.sha256" ) \
        || die "the published asset is not anonymously downloadable — friends can't get it"
    log "anonymous download + checksum OK"
else
    log "draft is up. Review it, then publish:"
    log "  gh release edit $TAG --repo $RELEASES_REPO --draft=false --latest=false"
fi

log "Done: https://github.com/$RELEASES_REPO/releases/tag/$TAG"
