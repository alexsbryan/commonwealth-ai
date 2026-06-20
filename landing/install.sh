#!/bin/sh
# Sovereign CLI installer.
#
#   curl -fsSL https://svrnme.sh/install.sh | sh
#
# Detects your platform, downloads the matching prebuilt CLI from GitHub
# Releases, verifies its checksum, and installs the binaries to ~/.local/bin.
#
# Env overrides:
#   SOVEREIGN_INSTALL_DIR   where to put the binaries (default: ~/.local/bin)
#   SOVEREIGN_VERSION       a release tag, e.g. cli-v0.1.0 (default: latest)
#
# The CLI is three binaries: `sovereign-cli` (the dispatcher you run as
# `sovereign`) plus the `sovereign-cli-daemon` and `sovereign-cli-llm` siblings
# it exec()s. They install together; `sovereign` is symlinked to the dispatcher.

set -eu

REPO="alexsbryan/commonwealth-ai"
BINS="sovereign-cli sovereign-cli-daemon sovereign-cli-llm"
INSTALL_DIR="${SOVEREIGN_INSTALL_DIR:-$HOME/.local/bin}"

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
      x86_64) err "no prebuilt for Intel macOS yet — build from source: https://github.com/$REPO" ;;
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
ver="${SOVEREIGN_VERSION:-latest}"
if [ "$ver" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$ver"
fi
tarball="sovereign-$target.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "downloading $tarball ($ver)"
curl -fsSL "$base/$tarball" -o "$tmp/$tarball" \
  || err "download failed: $base/$tarball (no release asset for $target yet?)"

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
ln -sf sovereign-cli "$INSTALL_DIR/sovereign"

say "installed sovereign → $INSTALL_DIR/sovereign"

# ── PATH hint + next step ────────────────────────────────────────────────
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\n  %s is not on your PATH. Add this to your shell profile:\n' "$INSTALL_DIR"
    printf '    export PATH="%s:$PATH"\n' "$INSTALL_DIR"
    ;;
esac

printf '\n  next:  sovereign setup\n\n'
