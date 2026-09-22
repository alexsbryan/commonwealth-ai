#!/usr/bin/env bash
# Build the guest runtime into landing/ring/ — the path svrnme.sh serves it at.
#
# `vercel.json`'s `trailingSlash: false` means /ring/ redirects to /ring and
# Vercel serves ring/index.html there; the guest link's page path is /ring/
# (guest_door::PAGE_PREFIX), so a link minted with `--url https://svrnme.sh/`
# lands on this directory.
#
# Run by `npm run deploy` (package.json), so a deploy always carries a fresh
# runtime rather than a stale committed binary. landing/ring/ is gitignored;
# if the deploy ever moves to Vercel's git integration, either commit that
# directory or give Vercel a buildCommand with a Rust + wasm-bindgen toolchain.
set -euo pipefail
cd "$(dirname "$0")/../.."
exec ./scripts/build-ring-runtime.sh --out landing/ring
