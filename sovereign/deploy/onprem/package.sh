#!/usr/bin/env bash
# package.sh — build the kit. Runs on OUR side, never on theirs.
#
#   ./package.sh --out /tmp/kits --version 2026.08.03
#
# This is where the air gap is actually solved. Everything the firm's box
# will ever need is resolved here, on a machine with a network, and
# shipped as one archive plus a manifest. Their box contacts nothing.
#
# Produces:
#   firm-rag-<version>.tar.zst
#   firm-rag-<version>.tar.zst.sha256
#
# ── The two build flags that matter ──────────────────────────────────
#   sovereign-server --no-default-features
#       drops `dev-routes` (the shell-reaching and absolute-path-ingesting
#       routes) AND `net-tools` (the three agent tools that reach the open
#       internet on ordinary chat turns). Both are default-ON so every
#       other build in the fleet is unchanged; this is the one build that
#       asks for neither.
#   sovereign-cli-daemon --features ocr
#       compiles in the PaddleOCR engine. Without it a scanned PDF is
#       reported as scanned_no_text and never indexed — and for a
#       litigation practice, scans ARE the corpus.
#
# `sovereign-server` is NOT in the standard release set
# (scripts/release-cli-local.sh builds sovereign-cli, -cli-daemon,
# -cli-llm). That is why this script builds it explicitly.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
KIT_SRC="$REPO_ROOT/sovereign/deploy/onprem"

OUT_DIR=""
VERSION=""
TARGET="x86_64-unknown-linux-gnu"
SKIP_CORPUS=0
SKIP_MODELS=0
MODELS_SRC="${MODELS_SRC:-$HOME/.svrnmesh/models}"

die() { printf 'package: %s\n' "$*" >&2; exit 1; }
say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

usage() {
    cat <<'EOF'
usage: ./package.sh --out <dir> --version <ver> [options]

  --out <dir>       where to write the archive
  --version <ver>   version string, e.g. 2026.08.03

  --target <triple> default x86_64-unknown-linux-gnu
  --models <dir>    GGUF source dir (default $HOME/.svrnmesh/models,
                    override with MODELS_SRC)
  --skip-corpus     do not build/publish the us-code snapshot
  --skip-models     do not stage GGUFs (a much smaller kit for a dry run)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --out)     OUT_DIR="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --target)  TARGET="${2:-}"; shift 2 ;;
        --models)  MODELS_SRC="${2:-}"; shift 2 ;;
        --skip-corpus) SKIP_CORPUS=1; shift ;;
        --skip-models) SKIP_MODELS=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage; die "unknown argument: $1" ;;
    esac
done

[ -n "$OUT_DIR" ] || { usage; die "--out is required"; }
[ -n "$VERSION" ] || { usage; die "--version is required"; }
command -v zstd >/dev/null || die "zstd is not installed"

STAGE="$(mktemp -d)"
KIT="$STAGE/firm-rag-$VERSION"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$KIT"/{bin,models,ocr,corpora,config,systemd,nginx}

# ── 1. Binaries ──────────────────────────────────────────────────────
# Release profile here, unlike day-to-day work in this repo: this is the
# artifact a firm runs for months, not an iteration.
say "building binaries ($TARGET)"
cd "$REPO_ROOT"

say "  sovereign-server --no-default-features"
cargo build --release --target "$TARGET" \
    -p sovereign-server --no-default-features
say "  sovereign-cli, -cli-daemon (--features ocr), -cli-llm"
cargo build --release --target "$TARGET" \
    -p sovereign-cli -p sovereign-cli-llm
cargo build --release --target "$TARGET" \
    -p sovereign-cli-daemon --features ocr

BIN="$REPO_ROOT/target/$TARGET/release"
# `svrn` IS sovereign-cli: the dispatcher resolves siblings by exact
# filename next to its own path, so the other three keep their names.
install -m 0755 "$BIN/sovereign-cli"        "$KIT/bin/svrn"
install -m 0755 "$BIN/sovereign-cli-daemon" "$KIT/bin/sovereign-cli-daemon"
install -m 0755 "$BIN/sovereign-cli-llm"    "$KIT/bin/sovereign-cli-llm"
install -m 0755 "$BIN/sovereign-server"     "$KIT/bin/sovereign-server"

# Prove the hardening is in the artifact HERE, rather than discovering it
# on their box.
#
# ── What this gate can and cannot see ────────────────────────────────
# Measured on a real `--no-default-features` build, because the first
# version of this gate was wrong twice and both failures were silent:
#
#   * `grep -qx` (whole-line match) NEVER matches. Rust packs string
#     literals into one blob, so `strings` emits them glued to their
#     neighbours; an exact-line match on any literal returns 0 whether
#     the code is compiled in or not. The original gate always passed
#     and therefore proved nothing. Substring matching is required.
#
#   * The ROUTE literals genuinely disappear (`/v1/solve` and
#     `/mcp/stats` → 0 substring matches; `/v1/conversations`, which IS
#     registered, → 1). They live in this crate behind `#[cfg]`, so a
#     hit is real evidence. That is a sound gate and it is enforced.
#
#   * The TOOL IDS do NOT disappear (`web_fetch` → 7 matches,
#     `wikipedia_fetch` → 2, on a correctly hardened build). They come
#     from `sovereign-tools`, which stays linked; `net-tools` gates the
#     REGISTRATION, not the type. Grepping for them here would refuse to
#     package a correct kit. That check is deliberately NOT made — the
#     sound proof is `acceptance.sh` check 0c, which enumerates
#     `GET /v1/tools` on the running server, and it runs on their box.
say "verifying the hardened build"
for sym in /v1/solve /v1/cycle/bdd /mcp/stats; do
    if LC_ALL=C strings -a "$KIT/bin/sovereign-server" 2>/dev/null | grep -q -- "$sym"; then
        die "sovereign-server still contains the route literal '$sym'.
     It was NOT built with --no-default-features. Refusing to package."
    fi
done
# Positive control: if this literal is ALSO absent, `strings` did not
# read the binary and the three checks above were vacuous. A gate that
# cannot fail is not a gate.
LC_ALL=C strings -a "$KIT/bin/sovereign-server" 2>/dev/null | grep -q -- "/v1/conversations" \
    || die "the control literal '/v1/conversations' is missing too, which means this
     check read nothing. Do not trust the three route assertions above."
echo "    dev-routes route literals absent (control literal present)"
echo "    net-tools: not checkable from the binary — acceptance.sh check 0c"
echo "               proves it at runtime on the target box"

# ── 2. Models ────────────────────────────────────────────────────────
if [ "$SKIP_MODELS" -eq 0 ]; then
    say "staging GGUFs from $MODELS_SRC"
    [ -d "$MODELS_SRC" ] || die "no model dir at $MODELS_SRC (set --models)"
    staged=0
    for f in "$MODELS_SRC"/*.gguf; do
        [ -e "$f" ] || continue
        cp "$f" "$KIT/models/"; staged=$((staged+1))
        echo "    $(basename "$f")"
    done
    [ "$staged" -gt 0 ] || die "no *.gguf found in $MODELS_SRC"
    # The embed model is not optional and not interchangeable: the
    # us-code snapshot restore hard-errors on a dimension mismatch, and a
    # same-dimension DIFFERENT model would restore cleanly and then return
    # quietly wrong neighbours forever.
    ls "$KIT/models" | grep -qi 'embedding' \
        || die "no embedding model among the staged GGUFs. The corpus snapshot
     cannot be restored without the model that built it."
else
    say "SKIPPING models (--skip-models)"
fi

# ── 3. OCR assets ────────────────────────────────────────────────────
# Files, not packages. This is the whole reason the engine is PaddleOCR
# and not tesseract: tesseract's install story is `apt install
# tesseract-ocr`, which is exactly what an air-gapped box cannot do.
# Paddle's dependencies are ~20 MB of files that go in the tarball.
say "OCR assets"
FETCH="$REPO_ROOT/scripts/fetch-desktop-binaries.sh"
if [ -x "$FETCH" ]; then
    "$FETCH" "$TARGET" || die "fetch-desktop-binaries.sh failed"
fi
DESKTOP_BIN="$REPO_ROOT/sovereign/crates/sovereign-desktop/src-tauri/binaries"
if [ -d "$DESKTOP_BIN/paddle-ocr" ]; then
    cp -a "$DESKTOP_BIN/paddle-ocr" "$KIT/ocr/"
    # A partial set does not half-work: build_engine refuses at ingest.
    for f in det.onnx rec.onnx dict.txt; do
        [ -f "$KIT/ocr/paddle-ocr/ppocr-en-v4v5/$f" ] \
            || die "OCR model set incomplete: $f missing"
    done
    cp "$DESKTOP_BIN/pdfium/libpdfium.so" "$KIT/ocr/libpdfium.so" \
        || die "libpdfium.so not found — without it no PDF can be rasterized
     and OCR produces nothing at all"
    echo "    paddle-ocr (12.6 MB) + libpdfium.so (7.6 MB)"
else
    die "no OCR assets at $DESKTOP_BIN/paddle-ocr. Run:
     $FETCH $TARGET"
fi

# ── 4. Corpus snapshot ───────────────────────────────────────────────
# us-code only. Not shipping scotus-opinions / olc-opinions (they need a
# CourtListener API token on a paid tier) or crs_reports (5 GB).
# Enrichment-disabled, which also sidesteps the ~38 s synchronous atlas
# parse on the first query after a restore.
if [ "$SKIP_CORPUS" -eq 0 ]; then
    say "publishing the us-code snapshot"
    "$BIN/sovereign-cli" corpus snapshot publish us-code \
        || die "snapshot publish failed — is the us-code corpus built and the daemon up?"
    snap="$(find "$HOME/.svrnmesh/snapshots" "$HOME/.svrnmesh/snapshots" \
              -name 'us-code*.tar.zst' -newermt '-10 minutes' 2>/dev/null | head -n1)"
    [ -n "$snap" ] || die "published, but could not locate the archive"
    cp "$snap" "$KIT/corpora/us-code.tar.zst"
    ( cd "$KIT/corpora" && sha256sum us-code.tar.zst > us-code.sha256 )
    echo "    $(du -h "$KIT/corpora/us-code.tar.zst" | cut -f1)"
else
    say "SKIPPING corpus (--skip-corpus)"
fi

# ── 5. Kit files ─────────────────────────────────────────────────────
say "kit files"
install -m 0755 "$KIT_SRC/install.sh"    "$KIT/install.sh"
install -m 0755 "$KIT_SRC/acceptance.sh" "$KIT/acceptance.sh"
install -m 0644 "$KIT_SRC/README.md"     "$KIT/README.md"
install -m 0644 "$KIT_SRC/EGRESS.md"     "$KIT/EGRESS.md"
install -m 0644 "$KIT_SRC/daemon-config.toml" "$KIT/config/daemon-config.toml"
install -m 0644 "$KIT_SRC/server-config.toml" "$KIT/config/server-config.toml"
install -m 0644 "$KIT_SRC/acceptance-probes.env.template" "$KIT/config/"
install -m 0644 "$KIT_SRC/systemd/"*.service "$KIT/systemd/"
install -m 0644 "$KIT_SRC/nginx/"*.conf     "$KIT/nginx/"

# ── 6. Manifest, then archive ────────────────────────────────────────
# install.sh REFUSES to run without a matching manifest. An air-gapped
# delivery only means something if the thing on the USB stick is the
# thing we built.
say "manifest"
( cd "$KIT" && find . -type f ! -name MANIFEST.sha256 -print0 \
    | sort -z | xargs -0 sha256sum > MANIFEST.sha256 )
echo "    $(wc -l < "$KIT/MANIFEST.sha256") files"

say "archiving"
mkdir -p "$OUT_DIR"
ARCHIVE="$OUT_DIR/firm-rag-$VERSION.tar.zst"
tar --zstd -cf "$ARCHIVE" -C "$STAGE" "firm-rag-$VERSION"
( cd "$OUT_DIR" && sha256sum "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256" )

cat <<EOF

$(say "packaged")
  $ARCHIVE  ($(du -h "$ARCHIVE" | cut -f1))
  $ARCHIVE.sha256

Read the sha256 to them over a channel that is not the one carrying the
archive. On their box:

  tar --zstd -xf firm-rag-$VERSION.tar.zst
  cd firm-rag-$VERSION
  sudo ./install.sh --docs /srv/firm-docs --hostname <fqdn>

EOF
