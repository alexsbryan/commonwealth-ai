#!/usr/bin/env bash
# build-ring-runtime.sh — build the guest page into a deployable static site.
#
# The output is ONE directory to upload to the guest runtime's static origin
# (today svrnme.sh): index.html + the wasm module and its glue. No server, no
# certificate, no data; see sovereign/apps/ring-runtime/README.md.
#
#   target/ring-runtime/site/   <- upload this
#
# Needs the wasm-bindgen CLI matching the crate's wasm-bindgen pin (0.2.128)
# and the wasm32-unknown-unknown target. Both are checked, not assumed.
set -euo pipefail
cd "$(dirname "$0")/.."

# The output directory is the deployable static site. Overridable so a consumer
# (landing/scripts/build-ring-runtime.sh → landing/ring/) can place it where
# its host serves it; default is this repo's target/.
OUT="target/ring-runtime/site"
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="${2:?--out needs a directory}"; shift 2 ;;
    *) echo "build-ring-runtime: unknown argument $1 (only --out <dir>)" >&2; exit 2 ;;
  esac
done

APP="sovereign/apps/ring-runtime"
WASM_BINDGEN_MIN="0.2.128"

if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$'; then
  echo "build-ring-runtime: the wasm target is not installed — run:" >&2
  echo "  rustup target add wasm32-unknown-unknown" >&2
  exit 3
fi
if ! command -v wasm-bindgen >/dev/null; then
  echo "build-ring-runtime: wasm-bindgen is not on PATH — run:" >&2
  echo "  cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_MIN}" >&2
  exit 3
fi
# A version skew between the CLI and the crate's wasm-bindgen is a loud,
# confusing failure at run time; say it here where it is cheap to fix.
cli="$(wasm-bindgen --version | awk '{print $2}')"
if [ "$cli" != "$WASM_BINDGEN_MIN" ]; then
  echo "build-ring-runtime: wasm-bindgen CLI is ${cli}, the crate pins \
=${WASM_BINDGEN_MIN} — install the matching CLI:" >&2
  echo "  cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_MIN} --force" >&2
  exit 3
fi

echo "building $APP for wasm32-unknown-unknown (release)…"
( cd "$APP" && cargo build --release --target wasm32-unknown-unknown )

rm -rf "$OUT"
mkdir -p "$OUT/wasm"
wasm-bindgen --target web --out-dir "$OUT/wasm" \
  "$APP/target/wasm32-unknown-unknown/release/ring_runtime.wasm"

# Shrink the module when binaryen is present. Optional, not required: the page
# works without it, and a missing optimizer must not fail a build. The size
# matters because the guest downloads it — a CDN will gzip it again on top.
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz -o "$OUT/wasm/ring_runtime_bg.wasm.opt" "$OUT/wasm/ring_runtime_bg.wasm" \
    && mv "$OUT/wasm/ring_runtime_bg.wasm.opt" "$OUT/wasm/ring_runtime_bg.wasm"
  echo "wasm-opt: -Oz applied"
else
  echo "wasm-opt: not on PATH (binaryen) — shipping the unoptimized module"
fi

# The version is the module's content hash, stamped into every shell URL and
# the service-worker cache name, so a rebuild invalidates a guest's cache
# instead of serving a stale runtime (sovereign/apps/ring-runtime/web/sw.js).
VERSION="$(cat "$OUT/wasm/ring_runtime_bg.wasm" "$OUT/wasm/ring_runtime.js" | sha256sum | cut -c1-12)"
cp "$APP/web/index.html" "$APP/web/app.js" "$APP/web/sw.js" "$OUT/"
sed -i "s/__VERSION__/${VERSION}/g" "$OUT/index.html" "$OUT/app.js" "$OUT/sw.js"

echo
echo "built: $OUT  (version ${VERSION})"
echo "  size: $(du -h "$OUT/wasm/ring_runtime_bg.wasm" | cut -f1) wasm"
echo "  1. upload that directory to the guest runtime's static origin"
echo "     (today svrnme.sh, beside the landing page and installers)"
echo "  2. point a grant at it:"
echo "       svrn mesh grant --model <id> --wall --ttl 2h --url https://<origin>/ --qr-svg wall-qr.svg"
echo "  Serve it with gzip/brotli on (the wasm compresses well) and, if the"
echo "  host allows, the headers index.html's meta tags ask for — a CDN gives"
echo "  both; nothing here depends on them."
