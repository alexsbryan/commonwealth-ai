#!/bin/sh
# svrnmesh CLI installer.
#
#   curl -fsSL https://svrnme.sh/install.sh | sh
#
# Detects your platform, downloads the matching prebuilt CLI from GitHub
# Releases, verifies its checksum, and installs the binaries to ~/.local/bin.
#
# Env overrides (legacy SOVEREIGN_* names still honored during the rebrand):
#   SVRNMESH_INSTALL_DIR   where to put the binaries (default: ~/.local/bin)
#   SVRNMESH_VERSION       a release tag, e.g. cli-v0.1.0 (default: latest)
#   SVRNMESH_REPO          owner/name to install from (default: the source repo)
#
# The CLI is three binaries: `sovereign-cli` (the dispatcher you run as
# `svrn`) plus the `sovereign-cli-daemon` and `sovereign-cli-llm` siblings
# it exec()s. They install together; `svrn` is symlinked to the dispatcher
# (a transitional `sovereign` alias is also installed for one release).

set -eu

# The release repo. commonwealth-ai is public, so its release assets are
# anonymously fetchable and releases publish there directly.
#
# SHELF_REPO is the retired `alexsbryan/svrnmesh-releases` indirection — a
# public shelf that existed only because the source repo used to be private.
# Kept as a READ-ONLY fallback so this one-liner keeps working until the
# first release is cut from the source repo; the script says so out loud
# when it falls back. Drop the line (or set SVRNMESH_FALLBACK_REPO="") once
# a cli-v* release exists on commonwealth-ai.
REPO="${SVRNMESH_REPO:-alexsbryan/commonwealth-ai}"
SHELF_REPO="${SVRNMESH_FALLBACK_REPO-alexsbryan/svrnmesh-releases}"
BINS="sovereign-cli sovereign-cli-daemon sovereign-cli-llm"
INSTALL_DIR="${SVRNMESH_INSTALL_DIR:-${SOVEREIGN_INSTALL_DIR:-$HOME/.local/bin}}"

say() { printf '  %s\n' "$1"; }
err() { printf 'install: %s\n' "$1" >&2; exit 1; }

# ── Detect platform → release target triple ──────────────────────────────
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
      *) err "no prebuilt for Linux/$arch yet — build from source: https://github.com/$REPO" ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      arm64 | aarch64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) err "no prebuilt for macOS/$arch — build from source: https://github.com/$REPO" ;;
    esac
    ;;
  *)
    err "unsupported OS '$os' — build from source: https://github.com/$REPO"
    ;;
esac

command -v curl >/dev/null 2>&1 || err "curl is required"
command -v tar  >/dev/null 2>&1 || err "tar is required"

# ── Resolve the download base ────────────────────────────────────────────
ver="${SVRNMESH_VERSION:-${SOVEREIGN_VERSION:-latest}}"

# Repos to try, in order: the source repo, then the retired shelf.
repos="$REPO"
if [ -n "${SHELF_REPO:-}" ]; then
  # An `&& …` one-liner would be the last command of the list and abort the
  # whole script under `set -e` whenever the shelf is disabled.
  repos="$repos $SHELF_REPO"
fi

# Newest cli-v* in one repo, by MAX SEMVER — not by list order.
# GitHub's /releases/latest is a single repo-global pointer shared with the
# desktop-v* stream, so it can resolve to a desktop release that carries no
# CLI tarball. And the /releases list, while it excludes drafts, is NOT
# reliably ordered newest-first here: every release shares an identical
# created_at (it derives from the tagged commit's date), so GitHub's ordering
# is an unstable tiebreak — `head -n1` handed installs an OLDER version during
# the post-publish replication window (the desktop updater hit the same bug,
# 2026-07-15). Sort the cli-v* tags by semver and take the highest. Numeric
# per-component sort (POSIX; no `sort -V`, which BSD/macOS sort lacks) so
# 0.1.20 > 0.1.9 correctly.
latest_cli() {
  curl -fsSL "https://api.github.com/repos/$1/releases" 2>/dev/null \
    | grep -oE 'cli-v[0-9]+\.[0-9]+\.[0-9]+' \
    | sed 's/^cli-v//' \
    | sort -t. -k1,1n -k2,2n -k3,3n \
    | tail -n1
}

if [ "$ver" = "latest" ]; then
  # Max semver ACROSS the repos, not "the first one that has any": the source
  # repo still carries stale cli-v0.1.19 tags from before the shelf existed,
  # so first-match-wins would install an OLDER CLI than the shelf's newest.
  # Reverse lives on each KEY (`nr`), not as a global `-r`: with per-key
  # ordering flags present, a global `-r` applies only to sort's last-resort
  # comparison and leaves the numeric keys ascending — which silently picked
  # 0.1.19 over 0.6.0 when this was written. `-s` then keeps input order among
  # equals, so a version present in both repos is taken from the source repo.
  best="$(
    for r in $repos; do
      num="$(latest_cli "$r")"
      # `if`, not `&&`: a failing `&&` list is the last command of the loop
      # body, so under `set -e` an empty first repo would abort the whole
      # substitution before the fallback repo is ever queried.
      if [ -n "$num" ]; then printf '%s %s\n' "$num" "$r"; fi
    done | sort -s -t. -k1,1nr -k2,2nr -k3,3nr | head -n1
  )"
  [ -n "$best" ] || err "no published cli-v* release found in: $repos (set SVRNMESH_VERSION=cli-vX.Y.Z to pin one)"
  ver="cli-v${best%% *}"
  repos="${best##* }"
fi
tarball="sovereign-$target.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "downloading $tarball ($ver)"
base=""
for r in $repos; do
  b="https://github.com/$r/releases/download/$ver"
  if curl -fsSL "$b/$tarball" -o "$tmp/$tarball" 2>/dev/null; then
    base="$b"
    # Never substitute silently: name the fallback when it is what answered.
    [ "$r" = "$REPO" ] || say "note: $ver came from the retired $r shelf"
    break
  fi
done
[ -n "$base" ] \
  || err "download failed: no $tarball for $ver in: $repos (no release asset for $target yet?)"

# ── Verify checksum against the release SHA256SUMS (if present) ───────────
if curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" 2>/dev/null; then
  want="$(grep " $tarball\$" "$tmp/SHA256SUMS" 2>/dev/null | awk '{print $1}' | head -n1)"
  if [ -n "${want:-}" ]; then
    if command -v shasum >/dev/null 2>&1; then
      got="$(shasum -a 256 "$tmp/$tarball" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
      got="$(sha256sum "$tmp/$tarball" | awk '{print $1}')"
    else
      got=""
    fi
    if [ -n "$got" ] && [ "$got" != "$want" ]; then
      err "checksum mismatch for $tarball (expected $want, got $got)"
    fi
    [ -n "$got" ] && say "checksum ok"
  fi
fi

# ── Extract + install ────────────────────────────────────────────────────
say "extracting"
tar -xzf "$tmp/$tarball" -C "$tmp"
src="$tmp/sovereign-$target"
[ -d "$src" ] || err "unexpected archive layout (no $src)"

mkdir -p "$INSTALL_DIR"
for b in $BINS; do
  [ -f "$src/$b" ] || err "binary missing from archive: $b"
  if command -v install >/dev/null 2>&1; then
    install -m 0755 "$src/$b" "$INSTALL_DIR/$b"
  else
    cp "$src/$b" "$INSTALL_DIR/$b" && chmod 0755 "$INSTALL_DIR/$b"
  fi
done
ln -sf sovereign-cli "$INSTALL_DIR/svrn"
# Transitional alias so existing scripts/muscle-memory keep working; dropped a
# release after the svrnmesh rebrand settles.
ln -sf sovereign-cli "$INSTALL_DIR/sovereign"

say "installed svrn → $INSTALL_DIR/svrn"

# ── PATH hint + next step ────────────────────────────────────────────────
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\n  %s is not on your PATH. Add this to your shell profile:\n' "$INSTALL_DIR"
    printf '    export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    ;;
esac

printf '\n  next:  svrn setup\n\n'
