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

APP="sovereign/apps/ring-runtime"
OUT="target/ring-runtime/site"
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
cp "$APP/web/index.html" "$OUT/"

echo
echo "built: $OUT"
echo "  1. upload that directory to the guest runtime's static origin"
echo "     (today svrnme.sh, beside the landing page and installers)"
echo "  2. point a grant at it:"
echo "       svrn mesh grant --model <id> --wall --ttl 2h --url https://<origin>/ --qr-svg wall-qr.svg"
