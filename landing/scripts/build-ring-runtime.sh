#!/usr/bin/env bash
# Build the guest runtime into landing/ring/ — the path svrnme.sh serves it at.
#
# The guest link's page path is /ring/ (guest_door::PAGE_PREFIX), so a link
# minted with `--url https://svrnme.sh/` lands on this directory.
#
# The trailing slash is LOAD-BEARING: index.html loads its runtime with
# `./app.js`, and a browser resolves that relative to the page URL's last
# segment. At /ring/ it is /ring/app.js; at /ring (slash stripped by a host's
# trailingSlash normalization) it is /app.js, which is a 404 — no runtime, and
# the page sits on its static "Connecting…" forever. This shipped on
# 2026-09-22 and a phone hung on the wall link; vercel.json now carries
# `redirects: /ring → /ring/` and NO global `trailingSlash`, and
# verify-ring-deployed.sh asserts the slash survives and the resolved asset is
# 200 (it reported "live" while the page was dead, because it followed the
# strip and only checked the final status).
#
# Run by `npm run deploy` (package.json), so a deploy always carries a fresh
# runtime rather than a stale committed binary. landing/ring/ is gitignored;
# if the deploy ever moves to Vercel's git integration, either commit that
# directory or give Vercel a buildCommand with a Rust + wasm-bindgen toolchain.
set -euo pipefail
cd "$(dirname "$0")/../.."
exec ./scripts/build-ring-runtime.sh --out landing/ring
